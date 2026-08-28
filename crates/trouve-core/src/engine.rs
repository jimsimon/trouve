//! The engine: workspaces, sessions, threads, and the agent loop.
//!
//! One `Engine` backs one server. Turns run as spawned tasks; progress is
//! reported exclusively through the event log. Worktree mutations are
//! serialized per session (threads share the session worktree, ADR 0003).

use std::collections::{HashMap, HashSet};

/// Marker prompt content for turns that attach to vendor-autonomous agent
/// activity instead of prompting the model. It is stored and rendered as the
/// turn's prompt, so it is written to read sensibly in a transcript.
pub const BACKGROUND_ATTACH_PROMPT: &str = "[background agent activity]";
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use futures::{FutureExt, StreamExt};
use trouve_agents::{
    AgentBackend, BackendCollaboratorAccess, BackendCollaboratorEvent, BackendError, BackendEvent,
    BackendPermission, BackendStartupActivity, BackendSteer, BackendTurn,
};
use trouve_protocol::{
    AgentPersona, ApprovalDecision, BranchList, CreateSessionRequest, CreateThreadRequest, Event,
    ForkCheckpointResponse, ProviderInfo, ProvidersResponse, RestoreDirection, Scope, Session,
    SessionDiffFileSummary, SessionDiffSummary, SessionFileDiff, Thread, ToolStatus, TurnAccepted,
    TurnPhase, UpdateSessionRequest, UpdateThreadRequest, UpsertProviderRequest, Usage, Workspace,
    WorkspaceListItem,
};
use trouve_providers::{Message, Provider, ProviderEvent, ToolSpec};

use crate::config::{Config, ProviderConfig};
use crate::permissions::{
    ApprovalHub, ApprovalResolution, Gate, QuestionHub, QuestionResolution, allow_key, gate,
};
use crate::store::{
    ArtifactCleanupClaim, ArtifactCleanupJob, CheckpointRow, PromptAcceptance,
    SessionPrVerificationIntent, Store,
};
use crate::tools::{
    AttachmentMaterialization, AttachmentMaterializationFile, BackgroundMutationLease,
    DeletedSessionCleanup, LocalToolExecutor, MaterializedAttachment, McpConfigMutation,
    McpConfigMutationOutcome, McpConfigMutationRequest, SessionRepositoryDiff,
    SessionRepositoryPush, ToolCtx, ToolExecutor, ToolResult, edit_strategy_for_model,
};
use crate::{context, git, new_id, personas};

/// Safety valve: maximum provider round-trips within a single turn.
const MAX_ITERATIONS: usize = 32;
/// Bound native provider fan-out so a malformed or over-eager response cannot
/// monopolize the runtime. Results are still written to the provider
/// transcript in request order.
const MAX_PARALLEL_TOOL_CALLS: usize = 8;
/// Keep one slow repository from delaying every session while still bounding
/// concurrent GitHub traffic from the durable PR-verification worker.
const MAX_PARALLEL_SESSION_PR_VERIFICATIONS: usize = 4;
/// Bound one session's sequential GitHub work so a large backlog cannot hold
/// its verification lane for an entire global poll cycle.
const MAX_SESSION_PR_VERIFICATIONS_PER_PASS: usize = 4;
/// A single successful creator call cannot nominate unbounded durable work.
const MAX_SESSION_PR_VERIFICATIONS_PER_CREATION_CALL: usize = 16;
/// Recoverable verification work expires deliberately rather than after a
/// brief outage-driven attempt count.
const SESSION_PR_VERIFICATION_RETENTION_DAYS: i64 = 7;
/// Missing PRs get a short propagation grace period; head movement gets a
/// longer local-synchronization grace period; transport failures use the full
/// retention window but still have a hard request ceiling.
const MAX_SESSION_PR_NOT_FOUND_ATTEMPTS: u32 = 8;
const MAX_SESSION_PR_HEAD_MOVED_ATTEMPTS: u32 = 12;
const MAX_SESSION_PR_REQUEST_ATTEMPTS: u32 = 48;
const SESSION_PR_AUTH_RETRY_SECONDS: i64 = 30;
const SESSION_PR_LEGACY_EVIDENCE_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
/// Repository identity changes rarely, but Git remotes remain mutable outside
/// trouve. Revalidate periodically without spawning Git on every list poll.
const WORKSPACE_LIST_CACHE_TTL: Duration = Duration::from_secs(30);
/// Repository identity is presentation metadata. Fall back to workspace
/// identity instead of allowing a hostile or unhealthy Git probe to stall a
/// workspace-list request indefinitely.
const WORKSPACE_REPOSITORY_IDENTITY_TIMEOUT: Duration = Duration::from_secs(2);
const PR_VERIFICATION_FAILURE_AUTH: &str = "authentication";
const PR_VERIFICATION_FAILURE_CONTENTION: &str = "contention";
const PR_VERIFICATION_FAILURE_EVIDENCE: &str = "evidence";
const PR_VERIFICATION_FAILURE_HEAD_MOVED: &str = "head_moved";
const PR_VERIFICATION_FAILURE_NOT_FOUND: &str = "not_found";
const PR_VERIFICATION_FAILURE_PERSISTENCE: &str = "persistence";
const PR_VERIFICATION_FAILURE_TRANSIENT: &str = "transient";
const MAX_ATTACHMENTS_PER_PROMPT: usize = 4;
const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_BYTES: usize = 20 * 1024 * 1024;
const MAX_ATTACHMENT_NAME_BYTES: usize = 1024;
const MAX_ATTACHMENT_MIME_BYTES: usize = 255;
/// Production tools observe `ToolCtx::cancel` and clean up promptly. This
/// bound prevents a third-party/custom executor that violates that contract
/// from wedging the dispatcher forever.
#[cfg(not(test))]
const TOOL_CANCEL_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(test)]
const TOOL_CANCEL_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

type NativeToolCallResult = Result<(String, Vec<trouve_providers::ToolImage>)>;

fn attachment_mime_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_'
        )
}

fn base64_sextet(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Validate the request envelope, including its exact decoded sizes, before
/// allocating decoded buffers or creating any durable cleanup/file state.
fn validate_attachment_uploads(
    uploads: &[trouve_protocol::AttachmentUpload],
) -> Result<(), EngineError> {
    if uploads.len() > MAX_ATTACHMENTS_PER_PROMPT {
        return Err(EngineError::BadRequest(format!(
            "a prompt may contain at most {MAX_ATTACHMENTS_PER_PROMPT} attachments"
        )));
    }

    let mut total_bytes = 0usize;
    for upload in uploads {
        if upload.name.is_empty()
            || upload.name.trim().is_empty()
            || upload.name.len() > MAX_ATTACHMENT_NAME_BYTES
            || upload.name == "."
            || upload.name == ".."
            || upload
                .name
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        {
            return Err(EngineError::BadRequest(
                "attachment name must be a non-empty filename of at most 1024 bytes without controls or path separators"
                    .into(),
            ));
        }
        let Some((mime_type, mime_subtype)) = upload.mime.split_once('/') else {
            return Err(EngineError::BadRequest(format!(
                "attachment {} has an invalid MIME type",
                upload.name
            )));
        };
        if upload.mime.len() > MAX_ATTACHMENT_MIME_BYTES
            || mime_type.is_empty()
            || mime_subtype.is_empty()
            || mime_subtype.contains('/')
            || !mime_type.bytes().all(attachment_mime_token_byte)
            || !mime_subtype.bytes().all(attachment_mime_token_byte)
        {
            return Err(EngineError::BadRequest(format!(
                "attachment {} has an invalid MIME type",
                upload.name
            )));
        }

        let data = upload.data.as_bytes();
        let padding = match data {
            [.., b'=', b'='] => 2usize,
            [.., b'='] => 1usize,
            _ => 0usize,
        };
        let payload_len = data.len().saturating_sub(padding);
        let canonical_tail = payload_len
            .checked_sub(1)
            .and_then(|index| data.get(index))
            .and_then(|byte| base64_sextet(*byte))
            .is_some_and(|sextet| match padding {
                2 => sextet & 0x0f == 0,
                1 => sextet & 0x03 == 0,
                _ => true,
            });
        let valid_base64 = !data.is_empty()
            && data.len() % 4 == 0
            && data[..payload_len]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
            && data[payload_len..].iter().all(|byte| *byte == b'=')
            && canonical_tail;
        if !valid_base64 {
            return Err(EngineError::BadRequest(format!(
                "attachment {}: invalid base64",
                upload.name
            )));
        }
        let decoded_bytes = data
            .len()
            .checked_div(4)
            .and_then(|groups| groups.checked_mul(3))
            .and_then(|bytes| bytes.checked_sub(padding))
            .ok_or_else(|| EngineError::BadRequest("attachment size overflow".into()))?;
        if decoded_bytes == 0 {
            return Err(EngineError::BadRequest(format!(
                "attachment {} is empty",
                upload.name
            )));
        }
        if decoded_bytes > MAX_ATTACHMENT_BYTES {
            return Err(EngineError::BadRequest(format!(
                "attachment {} exceeds {} MB",
                upload.name,
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            )));
        }
        total_bytes = total_bytes
            .checked_add(decoded_bytes)
            .ok_or_else(|| EngineError::BadRequest("attachment size overflow".into()))?;
        if total_bytes > MAX_ATTACHMENT_TOTAL_BYTES {
            return Err(EngineError::BadRequest(format!(
                "attachments exceed {} MB in total",
                MAX_ATTACHMENT_TOTAL_BYTES / (1024 * 1024)
            )));
        }
    }
    Ok(())
}

struct PreparedAttachmentCleanup {
    store: Store,
    executor: Arc<dyn ToolExecutor>,
    root: PathBuf,
    paths: Vec<PathBuf>,
    claim: Option<ArtifactCleanupClaim>,
    heartbeat_cancel: tokio_util::sync::CancellationToken,
    ownership_lost: tokio_util::sync::CancellationToken,
    armed: bool,
}

impl PreparedAttachmentCleanup {
    fn new(
        store: Store,
        executor: Arc<dyn ToolExecutor>,
        root: PathBuf,
        paths: Vec<PathBuf>,
        claim: Option<ArtifactCleanupClaim>,
    ) -> Self {
        let heartbeat_cancel = tokio_util::sync::CancellationToken::new();
        let ownership_lost = tokio_util::sync::CancellationToken::new();
        if let Some(claim) = claim.clone()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(maintain_artifact_cleanup_claim(
                store.clone(),
                claim,
                heartbeat_cancel.clone(),
                ownership_lost.clone(),
            ));
        }
        Self {
            store,
            executor,
            root,
            paths,
            claim,
            heartbeat_cancel,
            ownership_lost,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.heartbeat_cancel.cancel();
        self.armed = false;
    }

    fn claim(&self) -> Option<ArtifactCleanupClaim> {
        self.claim.clone()
    }
}

impl Drop for PreparedAttachmentCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.heartbeat_cancel.cancel();
        if self.ownership_lost.is_cancelled() {
            if let Some(claim) = self.claim.as_ref() {
                tracing::warn!(job_id = %claim.id, "staging cleanup claim was lost before rollback");
            }
            return;
        }
        if let Some(claim) = self.claim.as_ref() {
            match self.store.renew_artifact_cleanup_claim(claim) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(job_id = %claim.id, "staging cleanup claim was lost before rollback");
                    return;
                }
                Err(error) => {
                    tracing::error!(job_id = %claim.id, %error, "could not verify staging cleanup ownership before rollback");
                    return;
                }
            }
        }
        let cleanup = self
            .executor
            .rollback_attachment_files(&self.root, &self.paths);
        if let Some(claim) = self.claim.as_ref() {
            match cleanup {
                Ok(()) => {
                    if let Err(error) = self.store.complete_claimed_artifact_cleanup_job(claim) {
                        tracing::error!(%error, job_id = %claim.id, "failed to retire rolled-back attachment job");
                    }
                }
                Err(error) => {
                    let _ = self.store.fail_claimed_artifact_cleanup_job(claim, &error);
                    tracing::error!(%error, job_id = %claim.id, "failed to roll back staged attachment files");
                }
            }
        } else if let Err(error) = cleanup {
            tracing::error!(%error, "failed to roll back unstaged attachment files");
        }
    }
}

async fn maintain_artifact_cleanup_claim(
    store: Store,
    claim: ArtifactCleanupClaim,
    cancel: tokio_util::sync::CancellationToken,
    ownership_lost: tokio_util::sync::CancellationToken,
) {
    #[cfg(not(test))]
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
    #[cfg(test)]
    const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(25);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(HEARTBEAT_INTERVAL) => {
                match store.renew_artifact_cleanup_claim(&claim) {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(job_id = %claim.id, "artifact cleanup claim heartbeat lost ownership");
                        ownership_lost.cancel();
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(job_id = %claim.id, %error, "artifact cleanup claim heartbeat failed");
                        ownership_lost.cancel();
                        break;
                    }
                }
            }
        }
    }
}

async fn execute_artifact_cleanup_job(
    store: Store,
    executor: Arc<dyn ToolExecutor>,
    attachment_root: PathBuf,
    managed_worktree_root: PathBuf,
    job: ArtifactCleanupJob,
    index_hooks: bool,
) {
    let Some(claim) = job.claim() else {
        tracing::error!(job_id = %job.id, "refusing to execute an unclaimed artifact cleanup job");
        return;
    };
    let heartbeat_cancel = tokio_util::sync::CancellationToken::new();
    let ownership_lost = tokio_util::sync::CancellationToken::new();
    let heartbeat = tokio::spawn(maintain_artifact_cleanup_claim(
        store.clone(),
        claim.clone(),
        heartbeat_cancel.clone(),
        ownership_lost.clone(),
    ));
    let repository_for_gc = job.repository_path.clone();
    let mut result = if let Some(session_id) = job.session_id.as_deref() {
        match store.session(session_id) {
            Ok(Some(_)) => Err(format!(
                "refusing cleanup because session {session_id} is live again"
            )),
            Err(error) => Err(format!(
                "could not verify that session {session_id} remains deleted: {error:#}"
            )),
            Ok(None) => match store.list_sessions(None) {
                Err(error) => Err(format!(
                    "could not verify that deleted-session worktree has no live owner: {error:#}"
                )),
                Ok(sessions) => {
                    match (job.worktree_path.as_deref(), job.repository_path.as_deref()) {
                        (Some(worktree), Some(repository)) => {
                            if let Some(owner) = sessions
                                .iter()
                                .find(|session| session.worktree_path == worktree)
                            {
                                Err(format!(
                                    "refusing cleanup because worktree {worktree} is owned by live session {}",
                                    owner.id
                                ))
                            } else {
                                executor
                                    .cleanup_deleted_session(&DeletedSessionCleanup {
                                        managed_worktree_root,
                                        worktree: PathBuf::from(worktree),
                                        repository: PathBuf::from(repository),
                                        session_id: session_id.to_string(),
                                        attachment_root: attachment_root.clone(),
                                        attachment_paths: job
                                            .attachment_paths
                                            .iter()
                                            .map(PathBuf::from)
                                            .collect(),
                                        ownership_lost: ownership_lost.clone(),
                                    })
                                    .await
                            }
                        }
                        _ => Err("deleted-session cleanup job is missing artifact paths".into()),
                    }
                }
            },
        }
    } else {
        executor
            .cleanup_attachment_files(
                &attachment_root,
                job.attachment_paths.iter().map(PathBuf::from).collect(),
                ownership_lost.clone(),
            )
            .await
    };
    if result.is_ok() {
        result = match store.renew_artifact_cleanup_claim(&claim) {
            Ok(true) if !ownership_lost.is_cancelled() => Ok(()),
            Ok(_) => {
                ownership_lost.cancel();
                Err("artifact cleanup claim is no longer owned".into())
            }
            Err(error) => {
                ownership_lost.cancel();
                Err(format!(
                    "could not verify artifact cleanup claim before completion: {error:#}"
                ))
            }
        };
    }
    heartbeat_cancel.cancel();
    let _ = heartbeat.await;

    match result {
        Ok(()) => {
            if let Err(error) = store.complete_claimed_artifact_cleanup_job(&claim) {
                tracing::warn!(job_id = %job.id, %error, "failed to retire artifact cleanup job");
            }
            if index_hooks && let Some(repository) = repository_for_gc {
                crate::tools::gc_index_store_in_background(PathBuf::from(repository));
            }
        }
        Err(error) => {
            if let Err(store_error) = store.fail_claimed_artifact_cleanup_job(&claim, &error) {
                tracing::error!(
                    job_id = %job.id,
                    %error,
                    %store_error,
                    "artifact cleanup failed and its durable retry could not be updated"
                );
            } else {
                tracing::warn!(job_id = %job.id, %error, "artifact cleanup deferred for retry");
            }
        }
    }
}

/// Keep backend persistence efficient without making live SSE output feel
/// buffered. Bursts flush by count; sparse output flushes by this deadline.
const STREAM_EVENT_BATCH_MAX: usize = 64;
const STREAM_EVENT_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(25);
/// Give an already-queued steer a deterministic turn even when a backend
/// stream can produce output forever without yielding Pending.
const MAX_BACKEND_EVENTS_BEFORE_STEER: usize = 32;

/// SQLite's busy handler does not cover every `SQLITE_LOCKED` collision. A
/// checkpoint is post-response bookkeeping, so briefly retry those transient
/// conflicts instead of turning an otherwise successful model turn into a
/// failure. The total delay is bounded below one second.
const CHECKPOINT_SQLITE_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(10),
    Duration::from_millis(25),
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
];

/// Compact the transcript once its estimated size crosses this share of the
/// model's context window.
const COMPACTION_THRESHOLD: f64 = 0.8;

/// End-to-end budget for refreshing one GitHub host. This bounds how long a
/// stalled GraphQL request can retain the shared dashboard-cache lock.
const GITHUB_DASHBOARD_REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Let every visible client retain its 30-second background cadence while
/// collapsing slightly staggered requests into one upstream refresh. The
/// five-second margin avoids stretching a single client's normal cadence.
const GITHUB_DASHBOARD_REFRESH_FRESHNESS: std::time::Duration = std::time::Duration::from_secs(25);
// The title model independently bounds a cold sidecar start and decoding.
// Leave a little handoff margin beyond those combined budgets so this outer
// timeout does not silently replace a valid model request with heuristics.
const SESSION_TITLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
#[cfg(not(test))]
const MODEL_CATALOG_VALIDATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(test)]
const MODEL_CATALOG_VALIDATION_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// Codex collaborators inherit the root thread's MCP URL. Stable Codex sends
/// its vendor thread id in the MCP request metadata, and app-server separately
/// emits the matching `mcpToolCall` item id. Bound the rendezvous between those
/// two independently scheduled transports.
#[cfg(not(test))]
const CODEX_BRIDGE_METADATA_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const CODEX_BRIDGE_METADATA_WAIT_TIMEOUT: Duration = Duration::from_millis(100);

/// Bounded recursive delegation. Depth counts spawn edges from the root
/// conversation thread, so a value of four permits root → child → grandchild
/// → great-grandchild → great-great-grandchild. The active-tree cap prevents
/// breadth at several levels from multiplying without bound.
const MAX_SUBAGENT_DEPTH: usize = 4;
const MAX_CONCURRENT_CHILDREN: usize = 4;
const MAX_ACTIVE_DESCENDANTS: usize = 16;

const TURN_CONCURRENCY_ENV: &str = "TROUVE_TURN_CONCURRENCY";
const DEFAULT_TURN_CONCURRENCY: usize = 26;
const BACKGROUND_TURN_CONCURRENCY_ENV: &str = "TROUVE_BACKGROUND_TURN_CONCURRENCY";
const DEFAULT_BACKGROUND_TURN_CONCURRENCY: usize = 24;
const PROVIDER_TURN_CONCURRENCY_ENV: &str = "TROUVE_PROVIDER_TURN_CONCURRENCY";
const DEFAULT_PROVIDER_TURN_CONCURRENCY: usize = 18;
const PROVIDER_BACKGROUND_CONCURRENCY_ENV: &str = "TROUVE_PROVIDER_BACKGROUND_TURN_CONCURRENCY";
const DEFAULT_PROVIDER_BACKGROUND_CONCURRENCY: usize = 16;
/// Bounds durable task and thread setup bursts across planned review turns.
/// Permits are released before model dispatch, so this does not cap active
/// reviewer turns.
const PLANNED_TURN_SETUP_CONCURRENCY: usize = 24;

fn session_branch_name(title: &str, session_id: &str, derive_from_session_title: bool) -> String {
    let id = session_id.strip_prefix("se_").unwrap_or(session_id);
    let short_id = id.get(..6).unwrap_or(id);
    if derive_from_session_title {
        format!("trouve/{}-{short_id}", git::slugify(title))
    } else {
        format!("trouve/{short_id}")
    }
}

fn positive_limit_from_env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

async fn flush_backend_event_batch(
    store: &Store,
    scope: &Scope,
    events: &mut Vec<Event>,
) -> Result<()> {
    if !events.is_empty() {
        store
            .append_events_async(scope.clone(), std::mem::take(events))
            .await?;
    }
    Ok(())
}

fn backend_event_name(event: &BackendEvent) -> &'static str {
    match event {
        BackendEvent::SessionStarted { .. } => "session_started",
        BackendEvent::TextDelta(_) => "text_delta",
        BackendEvent::ProgressDelta(_) => "progress_delta",
        BackendEvent::ProgressCompleted => "progress_completed",
        BackendEvent::ThinkingDelta(_) => "thinking_delta",
        BackendEvent::ThinkingCompleted => "thinking_completed",
        BackendEvent::ToolStarted { .. } => "tool_started",
        BackendEvent::ToolOutput { .. } => "tool_output",
        BackendEvent::ToolCompleted { .. } => "tool_completed",
        BackendEvent::ApprovalNeeded { .. } => "approval_needed",
        BackendEvent::QuestionsNeeded { .. } => "questions_needed",
        BackendEvent::CommandsUpdated { .. } => "commands_updated",
        BackendEvent::TodosUpdated { .. } => "todos_updated",
        BackendEvent::UsageUpdated { .. } => "usage_updated",
        BackendEvent::CompactionStarted => "compaction_started",
        BackendEvent::CompactionCompleted => "compaction_completed",
        BackendEvent::CompactionFailed => "compaction_failed",
        BackendEvent::CollaboratorStarted { .. } => "collaborator_started",
        BackendEvent::CollaboratorEvent { .. } => "collaborator_event",
        BackendEvent::Completed { .. } => "completed",
    }
}

fn enforce_automated_review_backend_boundary(
    automated_review: bool,
    tools_enabled: bool,
    full_tool_bridge: bool,
    confined_read_only: bool,
    backend_id: &str,
) -> Result<()> {
    if automated_review && tools_enabled && !full_tool_bridge && !confined_read_only {
        bail!(
            "automated code review requires backend {backend_id} to use the full ToolExecutor bridge or enforce read-only confinement"
        );
    }
    Ok(())
}

fn vendor_tool_uses_automated_review_budget(
    tools_enabled: bool,
    tool: &str,
    first_start: bool,
) -> bool {
    // A backend that cannot remove native read/search tools is allowed to use
    // them during a logically tool-free turn under its read-only confinement.
    // Tool-enabled review turns retain their hard per-turn cap. ACP backends
    // may repeat a tool_call update with the same id as its state changes, so
    // charge the logical call only once.
    tools_enabled && first_start && !trouve_direct_bridge_call(tool)
}

struct BackendCollaboratorProjection {
    thread: Thread,
    mode: AgentPersona,
    turn: u64,
    /// Whether the durable parent-rail link for this collaborator has been
    /// published. Child activity can arrive before the provider's formal
    /// collaborator announcement, so projection existence alone is not proof
    /// that the parent has seen `SubagentSpawned`.
    spawn_link_published: bool,
    vendor_turn_id: Option<String>,
    thinking_level: Option<String>,
    last_user_message: Option<String>,
    pending_prompt: Option<String>,
    text: String,
    segment: String,
    usage: Usage,
    tool_calls: HashMap<String, (String, serde_json::Value)>,
    tool_started_at: HashMap<String, Instant>,
    /// Codex emits a presentation wrapper around every MCP call in addition
    /// to the canonical ToolExecutor lifecycle. Retain its ids only long
    /// enough to suppress output/completion after ownership is correlated.
    suppressed_bridge_calls: HashSet<String>,
    /// Vendor-native mutations retain the session execution lane from
    /// approval until the matching completion event.
    mutation_permits: HashMap<String, tokio::sync::OwnedRwLockWriteGuard<()>>,
    pending_approval: Option<PendingCollaboratorApproval>,
    approval_cancels: HashMap<String, tokio_util::sync::CancellationToken>,
    persisted: Vec<Event>,
    terminal: bool,
}

struct PendingCollaboratorApproval {
    thread: Thread,
    turn: u64,
    mode: AgentPersona,
    call_id: String,
    tool: String,
    args: serde_json::Value,
    responder: tokio::sync::oneshot::Sender<bool>,
}

struct PendingCodexVendorOwner {
    id: u64,
    sender: tokio::sync::oneshot::Sender<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexCallValidationOutcome {
    Matched,
    Mismatched,
}

struct PendingCodexCallValidation {
    id: u64,
    vendor_thread_id: String,
    sender: tokio::sync::oneshot::Sender<CodexCallValidationOutcome>,
}

#[derive(Default)]
struct ActiveCodexBridgeRoot {
    next_id: u64,
    /// Active-turn vendor thread/session id -> durable trouve thread id.
    vendor_threads: HashMap<String, String>,
    pending_vendor_owners: HashMap<String, Vec<PendingCodexVendorOwner>>,
    /// App-server wrapper item ids that arrived before their HTTP call.
    wrapper_owners: HashMap<String, String>,
    /// HTTP calls waiting for their app-server wrapper item.
    pending_calls: HashMap<String, PendingCodexCallValidation>,
    /// Successfully matched ids cannot authorize a second execution.
    consumed_calls: HashSet<String>,
    /// Timed-out/cancelled ids remain unusable for the rest of the root turn.
    retired_calls: HashSet<String>,
}

#[derive(Default)]
struct BridgedToolOwnerState {
    roots: HashMap<String, ActiveCodexBridgeRoot>,
}

enum CodexVendorOwnerRegistration {
    Immediate(String),
    Pending {
        id: u64,
        receiver: tokio::sync::oneshot::Receiver<String>,
    },
    InactiveRoot,
}

enum CodexCallValidationRegistration {
    Immediate,
    Pending {
        id: u64,
        receiver: tokio::sync::oneshot::Receiver<CodexCallValidationOutcome>,
    },
    InactiveRoot,
    UnknownOwner,
    MismatchedOwner,
    Replayed,
}

/// Authorizes Codex's inherited MCP requests with explicit vendor identity.
/// Payload equality is deliberately absent: byte-identical calls from sibling
/// collaborators must remain independently routable.
#[derive(Default)]
struct BridgedToolOwnerRouter {
    state: Mutex<BridgedToolOwnerState>,
}

impl BridgedToolOwnerRouter {
    fn begin_root(&self, root_thread_id: &str) {
        self.state
            .lock()
            .unwrap()
            .roots
            .insert(root_thread_id.to_string(), ActiveCodexBridgeRoot::default());
    }

    fn bind_vendor_thread(
        &self,
        root_thread_id: &str,
        vendor_thread_id: &str,
        owner_thread_id: &str,
    ) -> std::result::Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let Some(root) = state.roots.get_mut(root_thread_id) else {
            return Err(format!(
                "Codex bridge root turn {root_thread_id} is no longer active"
            ));
        };
        if let Some(existing) = root.vendor_threads.get(vendor_thread_id) {
            if existing == owner_thread_id {
                return Ok(());
            }
            return Err(format!(
                "Codex vendor thread {vendor_thread_id} is already bound to {existing}, not {owner_thread_id}"
            ));
        }
        root.vendor_threads
            .insert(vendor_thread_id.to_string(), owner_thread_id.to_string());
        if let Some(waiters) = root.pending_vendor_owners.remove(vendor_thread_id) {
            for waiter in waiters {
                let _ = waiter.sender.send(owner_thread_id.to_string());
            }
        }
        Ok(())
    }

    fn register_vendor_owner(
        &self,
        root_thread_id: &str,
        vendor_thread_id: &str,
    ) -> CodexVendorOwnerRegistration {
        let mut state = self.state.lock().unwrap();
        let Some(root) = state.roots.get_mut(root_thread_id) else {
            return CodexVendorOwnerRegistration::InactiveRoot;
        };
        if let Some(owner) = root.vendor_threads.get(vendor_thread_id) {
            return CodexVendorOwnerRegistration::Immediate(owner.clone());
        }
        root.next_id = root.next_id.wrapping_add(1);
        let id = root.next_id;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        root.pending_vendor_owners
            .entry(vendor_thread_id.to_string())
            .or_default()
            .push(PendingCodexVendorOwner { id, sender });
        CodexVendorOwnerRegistration::Pending { id, receiver }
    }

    fn abandon_vendor_owner(&self, root_thread_id: &str, vendor_thread_id: &str, id: u64) {
        let mut state = self.state.lock().unwrap();
        let Some(root) = state.roots.get_mut(root_thread_id) else {
            return;
        };
        if let Some(waiters) = root.pending_vendor_owners.get_mut(vendor_thread_id) {
            waiters.retain(|waiter| waiter.id != id);
            if waiters.is_empty() {
                root.pending_vendor_owners.remove(vendor_thread_id);
            }
        }
    }

    fn register_call_validation(
        &self,
        root_thread_id: &str,
        vendor_thread_id: &str,
        owner_thread_id: &str,
        call_id: &str,
    ) -> CodexCallValidationRegistration {
        let mut state = self.state.lock().unwrap();
        let Some(root) = state.roots.get_mut(root_thread_id) else {
            return CodexCallValidationRegistration::InactiveRoot;
        };
        if root
            .vendor_threads
            .get(vendor_thread_id)
            .map(String::as_str)
            != Some(owner_thread_id)
        {
            return CodexCallValidationRegistration::UnknownOwner;
        }
        if root.consumed_calls.contains(call_id)
            || root.retired_calls.contains(call_id)
            || root.pending_calls.contains_key(call_id)
        {
            return CodexCallValidationRegistration::Replayed;
        }
        if let Some(wrapper_owner) = root.wrapper_owners.get(call_id) {
            if wrapper_owner != vendor_thread_id {
                return CodexCallValidationRegistration::MismatchedOwner;
            }
            root.wrapper_owners.remove(call_id);
            root.consumed_calls.insert(call_id.to_string());
            return CodexCallValidationRegistration::Immediate;
        }
        root.next_id = root.next_id.wrapping_add(1);
        let id = root.next_id;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        root.pending_calls.insert(
            call_id.to_string(),
            PendingCodexCallValidation {
                id,
                vendor_thread_id: vendor_thread_id.to_string(),
                sender,
            },
        );
        CodexCallValidationRegistration::Pending { id, receiver }
    }

    fn announce_wrapper(
        &self,
        root_thread_id: &str,
        vendor_thread_id: &str,
        owner_thread_id: &str,
        call_id: &str,
    ) -> bool {
        if call_id.is_empty() {
            return false;
        }
        let mut state = self.state.lock().unwrap();
        let Some(root) = state.roots.get_mut(root_thread_id) else {
            return false;
        };
        if root
            .vendor_threads
            .get(vendor_thread_id)
            .map(String::as_str)
            != Some(owner_thread_id)
        {
            return false;
        }
        if root.consumed_calls.contains(call_id) || root.retired_calls.contains(call_id) {
            return false;
        }
        if let Some(pending) = root.pending_calls.remove(call_id) {
            if pending.vendor_thread_id == vendor_thread_id {
                root.consumed_calls.insert(call_id.to_string());
                let _ = pending.sender.send(CodexCallValidationOutcome::Matched);
            } else {
                let _ = pending.sender.send(CodexCallValidationOutcome::Mismatched);
                root.wrapper_owners
                    .insert(call_id.to_string(), vendor_thread_id.to_string());
            }
            return true;
        }
        match root.wrapper_owners.get(call_id) {
            Some(existing) => existing == vendor_thread_id,
            None => {
                root.wrapper_owners
                    .insert(call_id.to_string(), vendor_thread_id.to_string());
                true
            }
        }
    }

    fn abandon_call_validation(&self, root_thread_id: &str, call_id: &str, id: u64) {
        let mut state = self.state.lock().unwrap();
        let Some(root) = state.roots.get_mut(root_thread_id) else {
            return;
        };
        if root
            .pending_calls
            .get(call_id)
            .is_some_and(|pending| pending.id == id)
        {
            root.pending_calls.remove(call_id);
            root.retired_calls.insert(call_id.to_string());
        }
    }

    fn clear_root(&self, root_thread_id: &str) {
        self.state.lock().unwrap().roots.remove(root_thread_id);
    }
}

/// Return the canonical trouve tool nested in a vendor MCP presentation item.
/// User-configured MCP servers retain their vendor wrapper; only the reserved
/// first-party `trouve` server is projected through ToolExecutor instead.
fn trouve_bridge_wrapper_call<'a>(
    tool: &str,
    args: &'a serde_json::Value,
) -> Option<(&'a str, &'a serde_json::Value)> {
    if tool != "mcpToolCall" {
        return None;
    }
    let server = args
        .get("server")
        .or_else(|| args.get("serverName"))?
        .as_str()?;
    if server != "trouve" {
        return None;
    }
    let nested_tool = args
        .get("tool")
        .or_else(|| args.get("toolName"))?
        .as_str()?;
    let arguments = args.get("arguments")?;
    Some((nested_tool, arguments))
}

/// Claude reports first-party MCP calls under their direct MCP name. Their
/// authoritative execution enters `handle_tool_call`, so the mirrored vendor
/// lifecycle event must not reserve a second automated-review budget slot.
fn trouve_direct_bridge_call(tool: &str) -> bool {
    crate::mcp::split_tool_name(tool).is_some_and(|(server, _)| server == "trouve")
}

struct BackendApprovalOutcome {
    owner_thread_id: Option<String>,
    call_id: String,
    responder: tokio::sync::oneshot::Sender<bool>,
    approved: Result<bool>,
    /// Held from approval until the vendor reports tool completion. This is
    /// the fallback confinement mechanism for vendor protocols that cannot
    /// replace their mutation tools with trouve's full MCP bridge.
    mutation_permit: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

struct PendingApprovalCleanup {
    approvals: Arc<ApprovalHub>,
    store: Store,
    scope: Scope,
    thread_id: String,
    call_id: String,
    armed: bool,
    requested_persisted: bool,
}

impl Drop for PendingApprovalCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self
            .approvals
            .resolve(&self.thread_id, &self.call_id, ApprovalDecision::Deny);
        if !self.requested_persisted {
            return;
        }
        if let Err(error) = self.store.append_event(
            self.scope.clone(),
            Event::ApprovalResolved {
                call_id: self.call_id.clone(),
                decision: ApprovalDecision::Deny,
            },
        ) {
            tracing::error!(
                call_id = %self.call_id,
                %error,
                "failed to persist approval denial during backend cleanup"
            );
        }
    }
}

struct PendingQuestionCleanup {
    questions: Arc<QuestionHub>,
    store: Store,
    scope: Scope,
    thread_id: String,
    request_id: String,
    armed: bool,
    requested_persisted: bool,
}

impl Drop for PendingQuestionCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self
            .questions
            .resolve(&self.thread_id, &self.request_id, None);
        if !self.requested_persisted {
            return;
        }
        if let Err(error) = self.store.append_event(
            self.scope.clone(),
            Event::QuestionResolved {
                request_id: self.request_id.clone(),
                answers: None,
            },
        ) {
            tracing::error!(
                request_id = %self.request_id,
                %error,
                "failed to persist skipped question during cleanup"
            );
        }
    }
}

async fn deny_pending_backend_approvals(
    pending: &mut futures::stream::FuturesUnordered<
        futures::future::BoxFuture<'static, BackendApprovalOutcome>,
    >,
    root_cancels: &mut HashMap<String, tokio_util::sync::CancellationToken>,
    collaborators: &mut HashMap<String, BackendCollaboratorProjection>,
) {
    for cancel in root_cancels.drain().map(|(_, cancel)| cancel) {
        cancel.cancel();
    }
    for collaborator in collaborators.values_mut() {
        for cancel in collaborator
            .approval_cancels
            .drain()
            .map(|(_, cancel)| cancel)
        {
            cancel.cancel();
        }
    }
    while let Some(outcome) = pending.next().await {
        let _ = outcome.responder.send(false);
        // Dropping the outcome releases any mutation permit acquired before
        // cancellation won the cleanup race.
    }
}

enum BackendLoopInput {
    Event(Option<Result<BackendEvent, BackendError>>),
    Approval(BackendApprovalOutcome),
}

/// Native collaborators are real, navigable threads while the vendor owns
/// their current turn. Claiming them in the normal dispatcher registry makes
/// messages queue instead of racing a second turn against the same vendor
/// session. The parent dispatcher remains active for this guard's lifetime,
/// so adding/removing child claims cannot change session activity by itself.
struct BackendCollaboratorClaims<'a> {
    active_threads: &'a Mutex<HashMap<String, String>>,
    claimed: HashSet<String>,
}

impl<'a> BackendCollaboratorClaims<'a> {
    fn new(active_threads: &'a Mutex<HashMap<String, String>>) -> Self {
        Self {
            active_threads,
            claimed: HashSet::new(),
        }
    }

    /// Claim an existing collaborator only when no independent dispatcher
    /// already owns it. Never overwrite another claim: doing so would let our
    /// later release erase a live replacement turn.
    fn claim(&mut self, thread_id: &str, session_id: &str) -> bool {
        if self.claimed.contains(thread_id) {
            return true;
        }
        let mut active = self.active_threads.lock().unwrap();
        if active.contains_key(thread_id) {
            return false;
        }
        active.insert(thread_id.to_string(), session_id.to_string());
        self.claimed.insert(thread_id.to_string());
        true
    }

    /// Create and claim a new collaborator while holding the same registry
    /// lock used by prompt dispatch. `ThreadCreated` may be published inside
    /// `create`, but clients cannot claim the visible thread until this method
    /// has installed the provider's ownership entry.
    fn create_claimed_thread(
        &mut self,
        session_id: &str,
        create: impl FnOnce() -> Result<Thread, EngineError>,
    ) -> Result<Thread, EngineError> {
        let mut active = self.active_threads.lock().unwrap();
        let thread = create()?;
        if active.contains_key(&thread.id) {
            return Err(EngineError::Conflict(format!(
                "thread {} became active while its collaborator was being created",
                thread.id
            )));
        }
        active.insert(thread.id.clone(), session_id.to_string());
        self.claimed.insert(thread.id.clone());
        Ok(thread)
    }

    fn release(&mut self, thread_id: &str) {
        if self.claimed.remove(thread_id) {
            self.active_threads.lock().unwrap().remove(thread_id);
        }
    }
}

impl Drop for BackendCollaboratorClaims<'_> {
    fn drop(&mut self) {
        let mut active = self.active_threads.lock().unwrap();
        for thread_id in self.claimed.drain() {
            active.remove(&thread_id);
        }
    }
}

async fn flush_backend_collaborator_batches(
    store: &Store,
    collaborators: &mut HashMap<String, BackendCollaboratorProjection>,
) -> Result<()> {
    for collaborator in collaborators.values_mut() {
        flush_backend_event_batch(
            store,
            &Scope::Thread(collaborator.thread.id.clone()),
            &mut collaborator.persisted,
        )
        .await?;
    }
    Ok(())
}

#[derive(Clone)]
struct ProviderTurnCapacity {
    all: Arc<tokio::sync::Semaphore>,
    background: Arc<tokio::sync::Semaphore>,
    backoff: Arc<Mutex<ProviderBackoff>>,
}

#[derive(Default)]
struct ProviderBackoff {
    until: Option<Instant>,
    delay: std::time::Duration,
}

/// Capacity shared by interactive desktop turns, spawned agents, and
/// background review personas. Background work has a smaller second gate, so
/// it can never occupy all global/provider slots and starve the desktop.
struct TurnScheduler {
    all: Arc<tokio::sync::Semaphore>,
    background: Arc<tokio::sync::Semaphore>,
    planned_setups: Arc<tokio::sync::Semaphore>,
    provider_all_limit: usize,
    provider_background_limit: usize,
    providers: Mutex<HashMap<String, ProviderTurnCapacity>>,
}

struct TurnCapacityGuard {
    _permits: Vec<tokio::sync::OwnedSemaphorePermit>,
    wait_ms: u64,
}

impl TurnScheduler {
    fn new() -> Self {
        let all_limit = positive_limit_from_env(TURN_CONCURRENCY_ENV, DEFAULT_TURN_CONCURRENCY);
        let background_limit = positive_limit_from_env(
            BACKGROUND_TURN_CONCURRENCY_ENV,
            DEFAULT_BACKGROUND_TURN_CONCURRENCY,
        )
        .min(all_limit.saturating_sub(1).max(1));
        let provider_all_limit = positive_limit_from_env(
            PROVIDER_TURN_CONCURRENCY_ENV,
            DEFAULT_PROVIDER_TURN_CONCURRENCY,
        );
        let provider_background_limit = positive_limit_from_env(
            PROVIDER_BACKGROUND_CONCURRENCY_ENV,
            DEFAULT_PROVIDER_BACKGROUND_CONCURRENCY,
        )
        .min(provider_all_limit.saturating_sub(1).max(1));
        Self {
            all: Arc::new(tokio::sync::Semaphore::new(all_limit)),
            background: Arc::new(tokio::sync::Semaphore::new(background_limit)),
            planned_setups: Arc::new(tokio::sync::Semaphore::new(PLANNED_TURN_SETUP_CONCURRENCY)),
            provider_all_limit,
            provider_background_limit,
            providers: Mutex::new(HashMap::new()),
        }
    }

    async fn acquire_planned_setup(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<tokio::sync::OwnedSemaphorePermit> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn setup cancelled"),
            permit = self.planned_setups.clone().acquire_owned() => {
                permit.map_err(|_| anyhow!("turn setup scheduler closed"))
            }
        }
    }

    fn provider(&self, model: &str) -> ProviderTurnCapacity {
        let provider = model
            .split_once('/')
            .map_or(model, |(provider, _)| provider);
        self.providers
            .lock()
            .unwrap()
            .entry(provider.to_owned())
            .or_insert_with(|| ProviderTurnCapacity {
                all: Arc::new(tokio::sync::Semaphore::new(self.provider_all_limit)),
                background: Arc::new(tokio::sync::Semaphore::new(self.provider_background_limit)),
                backoff: Arc::new(Mutex::new(ProviderBackoff::default())),
            })
            .clone()
    }

    async fn acquire(
        &self,
        model: &str,
        background: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<TurnCapacityGuard> {
        let started = Instant::now();
        let provider = self.provider(model);
        let cooldown = provider
            .backoff
            .lock()
            .unwrap()
            .until
            .and_then(|until| until.checked_duration_since(Instant::now()));
        if let Some(cooldown) = cooldown {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("turn cancelled"),
                _ = tokio::time::sleep(cooldown) => {}
            }
        }
        let mut permits = Vec::with_capacity(if background { 4 } else { 2 });
        if background {
            permits.push(tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("turn cancelled"),
                permit = self.background.clone().acquire_owned() => {
                    permit.map_err(|_| anyhow!("background turn scheduler closed"))?
                }
            });
            permits.push(tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("turn cancelled"),
                permit = provider.background.clone().acquire_owned() => {
                    permit.map_err(|_| anyhow!("provider background scheduler closed"))?
                }
            });
        }
        permits.push(tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            permit = self.all.clone().acquire_owned() => {
                permit.map_err(|_| anyhow!("turn scheduler closed"))?
            }
        });
        permits.push(tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            permit = provider.all.clone().acquire_owned() => {
                permit.map_err(|_| anyhow!("provider turn scheduler closed"))?
            }
        });
        Ok(TurnCapacityGuard {
            _permits: permits,
            wait_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    }

    fn record_outcome(&self, model: &str, error: Option<&str>) {
        let provider = self.provider(model);
        let mut backoff = provider.backoff.lock().unwrap();
        let throttled = error.is_some_and(|error| {
            let error = error.to_ascii_lowercase();
            [
                "429",
                "rate limit",
                "too many requests",
                "overloaded",
                "resource exhausted",
                "capacity",
            ]
            .iter()
            .any(|needle| error.contains(needle))
        });
        if throttled {
            backoff.delay = if backoff.delay.is_zero() {
                std::time::Duration::from_secs(1)
            } else {
                (backoff.delay * 2).min(std::time::Duration::from_secs(30))
            };
            backoff.until = Some(Instant::now() + backoff.delay);
        } else if error.is_none() && backoff.until.is_none_or(|until| Instant::now() >= until) {
            backoff.delay /= 2;
            backoff.until = None;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    SessionDiffTooLarge(String),
    #[error("{0}")]
    AuthenticationRequired(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

fn github_engine_error(error: anyhow::Error) -> EngineError {
    if github_error_requires_reauthentication(&error) {
        EngineError::AuthenticationRequired(
            "GitHub permissions are missing or expired. Re-authenticate under Settings → Integrations to grant the required repository and organization-read permissions."
                .into(),
        )
    } else {
        EngineError::Internal(error)
    }
}

fn github_error_requires_reauthentication(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<octocrab::Error>()
            .is_some_and(octocrab_error_requires_reauthentication)
    })
}

fn octocrab_error_requires_reauthentication(error: &octocrab::Error) -> bool {
    match error {
        octocrab::Error::Graphql { source, .. } => {
            source.0.iter().any(graphql_error_requires_reauthentication)
        }
        octocrab::Error::GitHub { source, .. } => {
            source.status_code.as_u16() == 401 || github_authentication_message(&source.message)
        }
        _ => false,
    }
}

fn graphql_error_requires_reauthentication(error: &octocrab::GraphqlError) -> bool {
    if github_authentication_message(&error.message) {
        return true;
    }
    let Some(extensions) = error
        .extensions
        .as_ref()
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let authentication_code = ["type", "code"].iter().any(|key| {
        extensions
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_uppercase().as_str(),
                    "BAD_CREDENTIALS" | "INSUFFICIENT_SCOPES" | "UNAUTHORIZED"
                )
            })
    });
    authentication_code
        || ["scopes", "requiredScopes", "required_scopes"]
            .iter()
            .any(|key| extensions.get(*key).is_some_and(|value| !value.is_null()))
}

fn github_authentication_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "resource not accessible by integration",
        "required scopes",
        "insufficient scope",
        "insufficient oauth scope",
        "bad credentials",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn session_diff_executor_error(error: String) -> EngineError {
    if error.contains("too large to render") {
        EngineError::SessionDiffTooLarge(error)
    } else {
        EngineError::Internal(anyhow!(error))
    }
}

const THINKING_OPTION_KEYS: [&str; 4] =
    ["thinking_level", "reasoning_effort", "effort", "reasoning"];

fn validate_thinking_level(level: Option<&str>) -> Result<(), EngineError> {
    if level.is_some_and(|value| value.trim().is_empty()) {
        return Err(EngineError::BadRequest(
            "default_thinking_level must not be empty".into(),
        ));
    }
    Ok(())
}

fn validate_persona_id(id: &str) -> Result<(), EngineError> {
    if !personas::is_valid_persona_id(id) {
        return Err(EngineError::BadRequest(
            "persona id must be non-empty and [a-zA-Z0-9_-] only".into(),
        ));
    }
    Ok(())
}

fn has_thinking_option(options: &serde_json::Map<String, serde_json::Value>) -> bool {
    THINKING_OPTION_KEYS
        .iter()
        .any(|key| options.contains_key(*key))
        || options.contains_key("thinking_budget_tokens")
}

fn inherit_thinking_option(
    options: &mut serde_json::Map<String, serde_json::Value>,
    mode_level: Option<&str>,
    global_level: Option<&str>,
) {
    if has_thinking_option(options) {
        return;
    }
    if let Some(level) = mode_level.or(global_level) {
        options.insert(
            "thinking_level".into(),
            serde_json::Value::String(level.into()),
        );
    }
}

fn thinking_option_property(
    model: &trouve_protocol::ModelInfo,
) -> Option<(&'static str, &serde_json::Value, &[serde_json::Value])> {
    THINKING_OPTION_KEYS.iter().find_map(|key| {
        let property = model
            .options_schema
            .pointer(&format!("/properties/{key}"))?;
        let values = property["enum"].as_array()?;
        (values.len() > 1).then_some((*key, property, values.as_slice()))
    })
}

pub(crate) fn advertised_thinking_levels(model: &trouve_protocol::ModelInfo) -> Vec<&str> {
    thinking_option_property(model)
        .map(|(_, _, values)| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn advertised_thinking_budget(
    model: &trouve_protocol::ModelInfo,
) -> Option<(u64, Option<u64>)> {
    let property = model
        .options_schema
        .pointer("/properties/thinking_budget_tokens")?;
    matches!(property["type"].as_str(), Some("integer" | "number")).then(|| {
        (
            property["minimum"].as_u64().unwrap_or(1),
            property["maximum"].as_u64(),
        )
    })
}

fn parse_thinking_budget(value: &str) -> Option<u64> {
    if let Ok(value) = value.parse::<u64>() {
        return Some(value);
    }
    let (mantissa, exponent) = if let Some(index) = value.find(['e', 'E']) {
        let exponent_text = value.get(index + 1..)?;
        if exponent_text
            .bytes()
            .any(|byte| matches!(byte, b'e' | b'E'))
        {
            return None;
        }
        (&value[..index], exponent_text.parse::<i32>().ok()?)
    } else {
        (value, 0)
    };
    if mantissa.starts_with('-') {
        return None;
    }
    let mantissa = mantissa.strip_prefix('+').unwrap_or(mantissa);
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole);
    digits.push_str(fraction);
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    let fractional_digits = i32::try_from(fraction.len()).ok()?;
    let shift = exponent.checked_sub(fractional_digits)?;
    if shift >= 0 {
        return digits
            .parse::<u64>()
            .ok()?
            .checked_mul(10_u64.checked_pow(shift.try_into().ok()?)?);
    }
    let truncated = usize::try_from(shift.unsigned_abs()).ok()?;
    let integer_digits = digits.len().checked_sub(truncated)?;
    let integral = digits[integer_digits..].bytes().all(|byte| byte == b'0');
    integral.then_some(())?;
    digits[..integer_digits].parse::<u64>().ok()
}

/// Resolve the canonical inherited `thinking_level` key through a model's
/// advertised options schema. Unknown/unsupported levels fall back to the
/// model's schema default; models without an enum thinking knob drop the
/// inherited option entirely.
fn normalize_thinking_option(
    options: &mut serde_json::Map<String, serde_json::Value>,
    model: Option<&trouve_protocol::ModelInfo>,
) {
    let Some(canonical) = options.get("thinking_level").cloned() else {
        return;
    };
    let property = model.and_then(thinking_option_property);
    let Some((key, property, values)) = property else {
        if let Some(model) = model
            && let Some((minimum, maximum)) = advertised_thinking_budget(model)
        {
            // An explicit native budget wins over a legacy/inherited canonical
            // value, just as native enum options do below.
            if options.contains_key("thinking_budget_tokens") {
                options.remove("thinking_level");
                return;
            }
            let selected = canonical
                .as_str()
                .and_then(parse_thinking_budget)
                .filter(|value| *value >= minimum && maximum.is_none_or(|max| *value <= max))
                .or_else(|| {
                    model
                        .options_schema
                        .pointer("/properties/thinking_budget_tokens/default")
                        .and_then(serde_json::Value::as_u64)
                });
            options.remove("thinking_level");
            if let Some(selected) = selected {
                options.insert(
                    "thinking_budget_tokens".into(),
                    serde_json::Value::Number(selected.into()),
                );
            }
            return;
        }
        options.remove("thinking_level");
        return;
    };

    // A thread-level selection already stored under the model's native key
    // wins over the inherited canonical value.
    if key != "thinking_level" && options.contains_key(key) {
        options.remove("thinking_level");
        return;
    }

    let selected = canonical
        .as_str()
        .filter(|level| values.iter().any(|value| value.as_str() == Some(*level)))
        .map(String::from)
        .or_else(|| property["default"].as_str().map(String::from));
    options.remove("thinking_level");
    if let Some(selected) = selected {
        options.insert(key.into(), serde_json::Value::String(selected));
    }
}

fn thinking_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        serde_json::Value::Bool(value) => Some(if *value { "on" } else { "off" }.into()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Return the effective thinking selection that will be handed to the
/// provider. Explicit normalized options win; otherwise the advertised model
/// default describes the behavior obtained by omitting the option.
fn resolved_thinking_level(
    options: &serde_json::Map<String, serde_json::Value>,
    model: Option<&trouve_protocol::ModelInfo>,
) -> Option<String> {
    THINKING_OPTION_KEYS
        .iter()
        .find_map(|key| options.get(*key).and_then(thinking_value))
        .or_else(|| {
            options
                .get("thinking_budget_tokens")
                .and_then(thinking_value)
        })
        .or_else(|| {
            model
                .and_then(thinking_option_property)
                .and_then(|(_, property, _)| property.get("default"))
                .and_then(thinking_value)
        })
        .or_else(|| {
            model
                .and_then(|model| {
                    model
                        .options_schema
                        .pointer("/properties/thinking_budget_tokens/default")
                })
                .and_then(thinking_value)
        })
}

type GithubDashboardCacheHandle = Arc<tokio::sync::Mutex<crate::github::GitHubDashboardCache>>;
type GithubDashboardRefresh = (String, String, GithubDashboardCacheHandle);

const GITHUB_PR_DETAIL_CACHE_CAPACITY: usize = 48;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GithubPrDetailKey {
    host: String,
    repository: String,
    number: u64,
    head_sha: String,
}

impl GithubPrDetailKey {
    fn from_info(info: &trouve_protocol::PrInfo) -> Self {
        Self {
            host: info.host.to_ascii_lowercase(),
            repository: info.repository.to_ascii_lowercase(),
            number: info.number,
            head_sha: info
                .head_sha
                .clone()
                .unwrap_or_default()
                .to_ascii_lowercase(),
        }
    }
}

#[derive(Clone)]
struct CachedGithubPrDetail {
    detail: trouve_protocol::PrDetail,
    sections: HashSet<trouve_protocol::PrDetailSection>,
    last_access: u64,
}

#[derive(Default)]
struct GithubPrDetailCache {
    entries: HashMap<GithubPrDetailKey, CachedGithubPrDetail>,
    access_counter: u64,
}

impl GithubPrDetailCache {
    fn invalidate_pr(&mut self, info: &trouve_protocol::PrInfo) {
        let host = info.host.to_ascii_lowercase();
        let repository = info.repository.to_ascii_lowercase();
        self.entries.retain(|key, _| {
            key.host != host || key.repository != repository || key.number != info.number
        });
    }

    fn get(
        &mut self,
        key: &GithubPrDetailKey,
        sections: &HashSet<trouve_protocol::PrDetailSection>,
    ) -> Option<trouve_protocol::PrDetail> {
        let entry = self.entries.get_mut(key)?;
        if !sections.is_subset(&entry.sections) {
            return None;
        }
        self.access_counter = self.access_counter.wrapping_add(1);
        entry.last_access = self.access_counter;
        Some(entry.detail.clone())
    }

    fn loaded_sections(
        &self,
        key: &GithubPrDetailKey,
    ) -> HashSet<trouve_protocol::PrDetailSection> {
        self.entries
            .get(key)
            .map(|entry| entry.sections.clone())
            .unwrap_or_default()
    }

    fn detail(&self, key: &GithubPrDetailKey) -> Option<trouve_protocol::PrDetail> {
        self.entries.get(key).map(|entry| entry.detail.clone())
    }

    fn mark_stale(
        &mut self,
        key: &GithubPrDetailKey,
        sections: &HashSet<trouve_protocol::PrDetailSection>,
    ) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.sections.retain(|section| !sections.contains(section));
        }
    }

    fn merge(
        &mut self,
        requested_key: &GithubPrDetailKey,
        mut detail: trouve_protocol::PrDetail,
        mut sections: HashSet<trouve_protocol::PrDetailSection>,
    ) -> trouve_protocol::PrDetail {
        let overview_fetched = sections.contains(&trouve_protocol::PrDetailSection::Overview);
        sections.insert(trouve_protocol::PrDetailSection::Overview);
        let actual_key = GithubPrDetailKey::from_info(&detail.info);
        let same_head = actual_key == *requested_key;
        if same_head && let Some(previous) = self.entries.remove(requested_key) {
            if !sections.contains(&trouve_protocol::PrDetailSection::Conversation) {
                detail.comments = previous.detail.comments;
                detail.review_threads = previous.detail.review_threads;
                detail.reviews = previous.detail.reviews;
            }
            if !sections.contains(&trouve_protocol::PrDetailSection::Commits) {
                detail.commits = previous.detail.commits;
                detail.commit_count = previous.detail.commit_count;
            }
            if !sections.contains(&trouve_protocol::PrDetailSection::Files) {
                detail.files = previous.detail.files;
            }
            if !overview_fetched {
                detail.stack = previous.detail.stack;
            }
            sections.extend(previous.sections);
        } else {
            self.entries.remove(requested_key);
        }
        self.access_counter = self.access_counter.wrapping_add(1);
        self.entries.insert(
            actual_key,
            CachedGithubPrDetail {
                detail: detail.clone(),
                sections,
                last_access: self.access_counter,
            },
        );
        while self.entries.len() > GITHUB_PR_DETAIL_CACHE_CAPACITY {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
        detail
    }
}

struct SteerTurnCommand {
    content: String,
    attachments: Vec<trouve_protocol::Attachment>,
    attachment_rows: Vec<(trouve_protocol::Attachment, String)>,
    attachment_cleanup: PreparedAttachmentCleanup,
    response: tokio::sync::oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
struct ActiveTurnSteerer {
    turn: u64,
    sender: tokio::sync::mpsc::Sender<SteerTurnCommand>,
    mutation_lane_state: tokio::sync::watch::Sender<SteerMutationLaneState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SteerMutationLaneState {
    Idle,
    Waiting,
    Ended,
}

/// Remove only the registration installed by this backend turn. A cancelled
/// dispatcher can overlap the next turn's startup while its future unwinds.
struct ActiveTurnSteererGuard<'a> {
    registry: &'a Mutex<HashMap<String, ActiveTurnSteerer>>,
    thread_id: String,
    turn: u64,
}

async fn receive_steer_command(
    receiver: &mut Option<tokio::sync::mpsc::Receiver<SteerTurnCommand>>,
    pending: &mut Option<SteerTurnCommand>,
    backend_session_ready: bool,
) -> Option<SteerTurnCommand> {
    if !backend_session_ready {
        return std::future::pending().await;
    }
    if pending.is_some() {
        return pending.take();
    }
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn reserve_ready_steer_after_event_budget<T>(
    receiver: &mut Option<tokio::sync::mpsc::Receiver<T>>,
    pending: &mut Option<T>,
    consecutive_events: &mut usize,
) -> bool {
    if pending.is_some() {
        return true;
    }
    if *consecutive_events < MAX_BACKEND_EVENTS_BEFORE_STEER {
        return false;
    }
    *consecutive_events = 0;
    let Some(active_receiver) = receiver.as_mut() else {
        return false;
    };
    match active_receiver.try_recv() {
        Ok(command) => {
            *pending = Some(command);
            true
        }
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => false,
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
            *receiver = None;
            false
        }
    }
}

fn reject_pending_steer(pending: &mut Option<SteerTurnCommand>, reason: &str) {
    if let Some(SteerTurnCommand { response, .. }) = pending.take() {
        let _ = response.send(Err(reason.into()));
    }
}

impl Drop for ActiveTurnSteererGuard<'_> {
    fn drop(&mut self) {
        let mut registry = self.registry.lock().unwrap();
        let matching_turn = registry
            .get(&self.thread_id)
            .is_some_and(|active| active.turn == self.turn);
        let removed = if matching_turn {
            registry.remove(&self.thread_id)
        } else {
            None
        };
        drop(registry);
        if let Some(active) = removed {
            active
                .mutation_lane_state
                .send_replace(SteerMutationLaneState::Ended);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeTicketClaims {
    pub bridge_tools: bool,
    pub serve_approval: bool,
    pub correlate_codex_owner: bool,
}

struct ActiveBridgeTicket {
    root_thread_id: String,
    claims: BridgeTicketClaims,
}

struct DeletingSessionMarker<'a> {
    sessions: &'a Mutex<std::collections::HashSet<String>>,
    session_id: String,
}

impl Drop for DeletingSessionMarker<'_> {
    fn drop(&mut self) {
        self.sessions.lock().unwrap().remove(&self.session_id);
    }
}

#[derive(Clone)]
struct GlobalDefaults {
    model: String,
    thinking_level: Option<String>,
    permission_mode: trouve_protocol::PermissionMode,
}
struct WorkspaceListCacheEntry {
    item: WorkspaceListItem,
    refreshed_at: Instant,
}

#[derive(Clone)]
struct ReviewWorkspaceRegistrationCommit {
    workspace_id: String,
    mutated: bool,
}

#[derive(Default)]
pub(crate) struct ReviewWorkspaceRegistrationFence {
    committed: Mutex<Option<ReviewWorkspaceRegistrationCommit>>,
}

#[derive(Clone, Copy)]
struct AutomatedReviewToolBudget {
    limit: u64,
    remaining: u64,
    dispatcher_claimed: bool,
}

#[derive(Default)]
struct AutomatedReviewToolBudgets {
    active: Mutex<HashMap<String, AutomatedReviewToolBudget>>,
}

impl AutomatedReviewToolBudgets {
    fn arm(
        self: &Arc<Self>,
        thread_id: &str,
        limit: u64,
    ) -> Result<AutomatedReviewToolBudgetGuard> {
        let mut active = self.active.lock().unwrap();
        match active.entry(thread_id.to_string()) {
            std::collections::hash_map::Entry::Occupied(_) => {
                bail!("automated-review tool budget is already active for thread {thread_id}");
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(AutomatedReviewToolBudget {
                    limit,
                    remaining: limit,
                    dispatcher_claimed: false,
                });
            }
        }
        Ok(AutomatedReviewToolBudgetGuard {
            budgets: self.clone(),
            thread_id: thread_id.to_string(),
            owner: AutomatedReviewToolBudgetOwner::PreDispatch,
        })
    }

    fn claim_dispatch(self: &Arc<Self>, thread_id: &str) -> Option<AutomatedReviewToolBudgetGuard> {
        let mut active = self.active.lock().unwrap();
        let budget = active.get_mut(thread_id)?;
        if budget.dispatcher_claimed {
            return None;
        }
        budget.dispatcher_claimed = true;
        Some(AutomatedReviewToolBudgetGuard {
            budgets: self.clone(),
            thread_id: thread_id.to_string(),
            owner: AutomatedReviewToolBudgetOwner::Dispatcher,
        })
    }

    fn reserve(&self, thread_id: &str) -> Result<()> {
        let mut active = self.active.lock().unwrap();
        let Some(budget) = active.get_mut(thread_id) else {
            return Ok(());
        };
        if budget.remaining == 0 {
            bail!("code-review tool-call limit exceeded ({})", budget.limit);
        }
        budget.remaining -= 1;
        Ok(())
    }
}

pub(crate) struct AutomatedReviewToolBudgetGuard {
    budgets: Arc<AutomatedReviewToolBudgets>,
    thread_id: String,
    owner: AutomatedReviewToolBudgetOwner,
}

enum AutomatedReviewToolBudgetOwner {
    PreDispatch,
    Dispatcher,
}

impl Drop for AutomatedReviewToolBudgetGuard {
    fn drop(&mut self) {
        let mut active = self.budgets.active.lock().unwrap();
        let remove = match self.owner {
            AutomatedReviewToolBudgetOwner::PreDispatch => active
                .get(&self.thread_id)
                .is_some_and(|budget| !budget.dispatcher_claimed),
            AutomatedReviewToolBudgetOwner::Dispatcher => true,
        };
        if remove {
            active.remove(&self.thread_id);
        }
    }
}

pub struct Engine {
    pub(crate) store: Store,
    pub(crate) data_dir: PathBuf,
    pub(crate) config_dir: Option<PathBuf>,
    /// Cache repository identity so frequent workspace-list polls normally
    /// avoid Git while still observing external remote changes within a bound.
    workspace_list_cache: Mutex<HashMap<String, WorkspaceListCacheEntry>>,
    /// Deduplicate expired repository-identity probes per workspace. Entries
    /// are weak so closed or inactive workspaces do not grow this registry.
    workspace_list_refresh_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    /// Canonical provider/model rosters, metadata, and option-schema catalog
    /// shared by API providers and CLI backends. Explicit integrations may
    /// still contribute newly released or account-specific live models.
    model_catalog: Arc<trouve_providers::models_dev::ModelsDevCatalog>,
    providers: RwLock<HashMap<String, Arc<dyn Provider>>>,
    /// Providers registered programmatically (`with_provider`); preserved
    /// across config-driven registry reloads.
    injected_providers: Mutex<HashMap<String, Arc<dyn Provider>>>,
    /// External agent backends (Codex app-server, cursor-agent, Claude Code
    /// CLI), keyed by provider id like `providers`.
    backends: RwLock<HashMap<String, Arc<dyn AgentBackend>>>,
    /// Backends registered programmatically (`with_backend`); preserved
    /// across config-driven registry reloads.
    injected_backends: Mutex<HashMap<String, Arc<dyn AgentBackend>>>,
    /// Background-turn signal receivers awaiting a forwarder task. Provider
    /// reloads rebuild the backend registry, so receivers are handed off
    /// through this level-triggered intake instead of being wired once at
    /// startup; the pump spawned by `start_background_turn_listener` drains
    /// it whenever `background_turn_intake_notify` fires.
    background_turn_intake: Mutex<Vec<(String, tokio::sync::mpsc::Receiver<String>)>>,
    background_turn_intake_notify: Arc<tokio::sync::Notify>,
    pub(crate) executor: Arc<dyn ToolExecutor>,
    approvals: Arc<ApprovalHub>,
    questions: Arc<QuestionHub>,
    turn_scheduler: TurnScheduler,
    /// Hard per-turn call caps for disposable automated-review threads.
    /// Reservations happen inside the complete tool dispatch boundary, so a
    /// parallel batch cannot race past its reviewer/coordinator allowance.
    automated_review_tool_budgets: Arc<AutomatedReviewToolBudgets>,
    /// Per-session worktree access. Read-only turns share a read guard;
    /// mutating turns and checkpoint restoration take the
    /// write guard. This lets review/plan work fan out without weakening the
    /// sessions-own-worktrees serialization invariant.
    session_locks: Mutex<HashMap<String, Arc<tokio::sync::RwLock<()>>>>,
    /// Serializes retries for one client-generated session-create key before
    /// any worktree mutation or event-writer batch is started.
    session_create_locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    /// Narrower execution lanes for tools operating on a session worktree.
    /// Read-only tools may overlap; every potential mutation is exclusive.
    /// Weak entries keep completed/deleted sessions from growing this map.
    tool_execution_locks: Mutex<HashMap<String, Weak<tokio::sync::RwLock<()>>>>,
    /// Serializes durable PR-intent reconciliation per session. Weak entries
    /// avoid retaining deleted sessions while still preventing duplicate
    /// GitHub reads and association events from overlapping retry triggers.
    session_pr_verification_locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    session_pr_verification_wake: Arc<tokio::sync::Notify>,
    session_pr_verification_worker_started: AtomicBool,
    /// Per-root delegation lanes. Providers may request multiple spawn tools
    /// in one parallel batch; serializing only the admission/create window
    /// for one tree makes the depth and active-descendant caps atomic without
    /// blocking unrelated subagent trees.
    subagent_tree_locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    /// Threads with a dispatcher currently running turns, mapped to their
    /// session. A thread in this map drains its own prompt queue; sends
    /// while present just enqueue. The session ids feed `Session.active`
    /// and the `session.activity` server event.
    active_threads: Mutex<std::collections::HashMap<String, String>>,
    /// Orders active-thread transitions with their persisted session activity
    /// events without holding `active_threads` across durable event appends.
    session_activity_publication: Mutex<()>,
    /// Serializes relational prompt-queue mutations with the full queue
    /// snapshots published on the durable thread stream. Lock ordering is
    /// activity publication -> prompt_queue_mutations -> active_threads.
    prompt_queue_mutations: Mutex<()>,
    /// Sessions currently being deleted. Dispatch checks this while holding
    /// `active_threads`, making "no active turns" and "no new turns" one
    /// atomic state transition before destructive cleanup begins.
    deleting_sessions: Mutex<std::collections::HashSet<String>>,
    /// Cancellation tokens for in-flight turns, keyed by thread id. Set while
    /// a turn runs; `cancel_turn` trips one to interrupt the turn's provider
    /// stream, tool calls, and approval waits at the next await point.
    turn_cancels: Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    /// Capability-aware channels into backend loops that can accept
    /// additional user input during an active turn. The turn number protects
    /// cleanup from removing a newer registration for the same thread.
    turn_steerers: Mutex<HashMap<String, ActiveTurnSteerer>>,
    /// Threads where a new prompt arrived after cancellation was requested.
    /// The cancelling dispatcher consumes this marker and resumes the queue
    /// instead of leaving that explicitly submitted follow-up paused.
    resume_after_cancel: Mutex<HashSet<String>>,
    /// Per-host incremental PR snapshots. The map lock is held only for entry
    /// management and identity validation; each host has its own async lock so
    /// network refreshes do not block host removal or unrelated hosts.
    github_dashboard_caches: Mutex<HashMap<String, GithubDashboardCacheHandle>>,
    /// Selected-PR page state, bounded and keyed by immutable head SHA. Tabs
    /// add sections to an entry on demand instead of repeating GitHub's rich
    /// nested detail query on every render or pane switch.
    github_pr_detail_cache: Mutex<GithubPrDetailCache>,
    /// Orders authenticated-host capture, cache registration, and snapshot
    /// publication against host removal without holding the cache map lock
    /// across event-log writes.
    github_dashboard_publication: Mutex<()>,
    /// Serializes provider upserts and deletions across config, secret-store,
    /// and registry mutations without blocking unrelated provider ids.
    provider_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Serializes persona-file mutations with durable deletion replay so a
    /// recreate cannot race a pending cleanup of the same user-level file.
    pub(crate) persona_mutations: Arc<tokio::sync::Mutex<()>>,
    pub(crate) config: Mutex<Config>,
    /// Where provider configuration changes are persisted. `None` disables
    /// persistence (tests).
    config_file: Option<PathBuf>,
    /// Serializes persistence and runtime application of title-model behavior.
    title_model_behavior_transition: tokio::sync::Mutex<()>,
    /// One coherent snapshot of the defaults inherited by personas. Keeping
    /// these values under one lock prevents new threads from observing a
    /// partially applied global-defaults update.
    global_defaults: RwLock<GlobalDefaults>,
    pub(crate) secrets: Arc<dyn trouve_providers::secrets::SecretStore>,
    pub(crate) code_review: crate::review::CodeReviewRuntime,
    /// In-flight OAuth logins, keyed by provider id.
    logins: Mutex<HashMap<String, LoginState>>,
    /// In-flight managed vendor-CLI installs, keyed by CLI id.
    cli_installs: Mutex<HashMap<String, CliInstallState>>,
    /// The llama-server sidecar behind the built-in "local" provider.
    local_manager: Arc<crate::local::LlamaManager>,
    /// A separate sidecar for session titles with independently configured
    /// resource placement.
    title_model: Arc<crate::title_model::TitleModelManager>,
    /// A timed-out generation stays tracked while its cold start finishes.
    /// The next request cancels and joins it before starting another.
    title_model_generation: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The built-in "local" provider, kept around so enabling re-injects
    /// the same instance after a disable removed it from the registry.
    local_provider: Arc<dyn Provider>,
    /// In-flight local model (GGUF) downloads, keyed by model id.
    local_downloads: Mutex<HashMap<String, LocalDownloadState>>,
    /// RAM/VRAM probe, run once on first use.
    hardware: std::sync::OnceLock<crate::local::Hardware>,
    /// Models already reported as lacking a context window. Missing metadata
    /// disables automatic compaction, but should not spam every turn.
    compaction_warnings: Mutex<std::collections::HashSet<String>>,
    /// Latest-version lookups, cached per CLI (network is best-effort).
    cli_latest: Mutex<HashMap<String, (std::time::Instant, Option<String>)>>,
    /// This server's reachable base URL (e.g. "http://127.0.0.1:7433"), set
    /// once the listener binds; the MCP tool bridge dials back through it.
    base_url: RwLock<Option<String>>,
    /// Ephemeral credential appended only to internal MCP bridge URLs.
    bridge_token: RwLock<Option<String>>,
    /// Per-thread capabilities layered over the process-wide bridge
    /// credential. Query flags and URL paths are never authorities by
    /// themselves; the opaque ticket binds their exact claims. Tickets remain
    /// stable across resumed vendor turns because Codex retains its MCP
    /// client, but validation also requires the owning thread to be active.
    bridge_tickets: Mutex<HashMap<String, ActiveBridgeTicket>>,
    /// Routes Codex MCP calls from the inherited root URL to the root or
    /// collaborator thread identified by app-server's item lifecycle.
    bridged_tool_owners: BridgedToolOwnerRouter,
    /// Warm the search index on session creation and GC the shared index
    /// store on archive/delete. Off by default so tests never touch the
    /// embedding model; the server enables it (`with_index_hooks`).
    index_hooks: bool,
    /// Per-server MCP logs, shared with the executor's `McpManager` so both
    /// runtime connections and settings health probes land in one place.
    mcp_logs: crate::mcp::McpLogStore,
    /// Interactive shells (one per session) for the client terminal panel.
    terminals: Arc<crate::terminal::TerminalManager>,
    /// Whether the server can reach the internet. Defaults to true; only a
    /// configured probe (`with_connectivity_probe`) or `set_online` ever
    /// flips it, so probe-less engines (tests, embedders) never go offline.
    online: std::sync::atomic::AtomicBool,
    /// Reachability check driven by the connectivity monitor. `None`
    /// disables monitoring entirely.
    connectivity_probe: Option<crate::connectivity::Probe>,
}

/// Keeps an idempotent session-create reservation alive with the detached
/// worktree attempt. Field order is intentional: if the request awaiting the
/// task is cancelled, the worktree receipt rolls back before the key guard is
/// released to a retry.
struct SessionWorktreeCreateAttempt {
    creation: Result<crate::tools::SessionWorktreeCreation, String>,
    idempotency_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

#[derive(Debug, Clone)]
enum LoginState {
    /// In flight; carries what the user was told to do so a repeated
    /// start_login can re-present it (e.g. after closing the browser tab)
    /// instead of refusing while the flow is still valid.
    Pending {
        started: trouve_protocol::LoginStarted,
        callback_sender: Option<tokio::sync::mpsc::Sender<String>>,
    },
    Success,
    Failed(String),
}

#[derive(Debug, Clone)]
enum CliInstallState {
    Pending {
        /// Version being installed, once discovered.
        version: Option<String>,
        /// Byte progress + cancel flag, shared with the install task.
        progress: Arc<trouve_agents::install::Progress>,
    },
    Success(String),
    Failed(String),
}

#[derive(Debug, Clone)]
enum LocalDownloadState {
    Pending {
        /// Bytes downloaded so far; the task updates the counter.
        bytes: Arc<std::sync::atomic::AtomicU64>,
        /// Set to make the download task stop and clean up its .part file.
        cancel: Arc<std::sync::atomic::AtomicBool>,
    },
    Failed(String),
}

/// Whether a `--version` report refers to the given vendor version. The
fn terminal_info(terminal: &crate::terminal::Terminal) -> trouve_protocol::TerminalInfo {
    let (cols, rows) = terminal.size();
    trouve_protocol::TerminalInfo {
        id: terminal.id.clone(),
        session_id: terminal.session_id.clone(),
        cols,
        rows,
        exited: terminal.exited(),
    }
}

/// CLIs decorate their output differently ("2.1.34 (Claude Code)",
/// "codex-cli 0.143.0", "2026.07.01-41b2de7"), so containment beats
/// equality.
fn cli_version_matches(reported: &str, version: &str) -> bool {
    reported == version
        || reported
            .split([' ', '(', ')'])
            .any(|tok| tok == version || tok.strip_prefix('v') == Some(version))
}

/// The managed CLI serving a backend provider kind, if any.
fn cli_for_kind(kind: &str) -> Option<trouve_agents::install::CliId> {
    use trouve_agents::install::CliId;
    match kind {
        "cursor-cli" => Some(CliId::CursorAgent),
        "claude-cli" => Some(CliId::Claude),
        "codex-app-server" => Some(CliId::Codex),
        _ => None,
    }
}

/// Resolve the executable for a CLI-backed provider. An explicit command
/// wins; otherwise a trouve-managed binary takes precedence over PATH.
fn resolved_cli_command(kind: &str, command: Option<String>, data_dir: &Path) -> Option<String> {
    command.or_else(|| {
        cli_for_kind(kind)
            .map(|cli| trouve_agents::install::managed_bin(data_dir, cli))
            .filter(|bin| bin.exists())
            .map(|bin| bin.to_string_lossy().into_owned())
    })
}

/// Config kinds handled by the [`AgentBackend`] seam rather than a Provider.
fn is_backend_kind(kind: &str) -> bool {
    matches!(kind, "codex-app-server" | "cursor-cli" | "claude-cli")
}

/// Config kinds whose auth lives in a vendor CLI.
fn is_cli_auth_kind(kind: &str) -> bool {
    is_backend_kind(kind)
}

/// Credential style for a configured provider: "cli" for vendor-CLI-backed
/// kinds, "oauth" when subscription endpoints are configured (and no inline
/// key wins), "none" for keyless local endpoints, "api-key" otherwise.
fn provider_auth_kind(pc: &ProviderConfig) -> String {
    if pc.kind == "amazon-bedrock" {
        "aws".into()
    } else if matches!(
        pc.kind.as_str(),
        "google-vertex" | "google-vertex-anthropic"
    ) {
        "gcp".into()
    } else if is_cli_auth_kind(&pc.kind) {
        // cursor-cli works both ways: subscription login ("cursor" preset)
        // or an API key ("cursor-api" preset, usage-based billing).
        if pc.kind == "cursor-cli" && (pc.api_key.is_some() || pc.api_key_env.is_some()) {
            "api-key".into()
        } else {
            "cli".into()
        }
    } else if pc.oauth.is_some() && pc.api_key.is_none() {
        "oauth".into()
    } else if pc.api_key.is_none()
        && pc.api_key_env.is_none()
        && pc.base_url.as_deref().is_some_and(is_loopback_base_url)
    {
        "none".into()
    } else {
        "api-key".into()
    }
}

/// Whether a provider endpoint lives on this machine (Ollama, llama.cpp,
/// vLLM, …) and therefore keeps working without internet. Parses the
/// authority and requires the exact host `localhost` or a loopback IP —
/// a substring check would also accept remote hosts like
/// `localhost.attacker.example` and mislabel them as offline-capable
/// keyless endpoints.
fn is_loopback_base_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // IPv6 hosts come back bracketed; IpAddr parsing wants them bare.
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Build the provider registry from config + zero-config env defaults.
fn build_all_providers(
    config: &Config,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
    catalog: &Arc<trouve_providers::models_dev::ModelsDevCatalog>,
) -> HashMap<String, Arc<dyn Provider>> {
    let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
    for (id, pc) in &config.providers {
        if is_backend_kind(&pc.kind) {
            continue; // handled by build_all_backends
        }
        match build_provider(id, pc, secrets, catalog) {
            Ok(p) => {
                providers.insert(id.clone(), p);
            }
            Err(e) => tracing::warn!("provider {id}: {e}; skipping"),
        }
    }
    // Zero-config defaults from conventional env vars.
    if !providers.contains_key("openai")
        && let Ok(p) = trouve_providers::openai_compat::OpenAiCompatProvider::openai_from_env()
    {
        providers.insert("openai".into(), Arc::new(p.with_catalog(catalog.clone())));
    }
    if !providers.contains_key("anthropic")
        && let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
    {
        providers.insert(
            "anthropic".into(),
            Arc::new(
                trouve_providers::anthropic::AnthropicProvider::new(
                    "anthropic",
                    None,
                    Arc::new(trouve_providers::auth::StaticToken(key)),
                )
                .with_catalog(catalog.clone()),
            ),
        );
    }
    providers
}

/// Build the agent-backend registry from config.
fn build_all_backends(
    config: &Config,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
    data_dir: &Path,
    catalog: &Arc<trouve_providers::models_dev::ModelsDevCatalog>,
) -> HashMap<String, Arc<dyn AgentBackend>> {
    let mut backends: HashMap<String, Arc<dyn AgentBackend>> = HashMap::new();
    for (id, pc) in &config.providers {
        // Explicit command wins; otherwise a trouve-managed install beats
        // whatever is on PATH (distro packages lag behind vendor releases).
        let command = resolved_cli_command(&pc.kind, pc.command.clone(), data_dir);
        let backend: Arc<dyn AgentBackend> = match pc.kind.as_str() {
            "codex-app-server" => Arc::new(
                trouve_agents::codex::CodexBackend::new(id, command).with_catalog(catalog.clone()),
            ),
            "cursor-cli" => {
                // Same precedence as native providers: inline key > env var >
                // key saved through settings (secret store). Subscription
                // login via the CLI still works when all are absent.
                let api_key = pc
                    .api_key
                    .clone()
                    .or_else(|| pc.api_key_env.as_ref().and_then(|v| std::env::var(v).ok()))
                    .or_else(|| {
                        secrets
                            .get(&trouve_providers::secrets::api_key_secret(id))
                            .ok()
                            .flatten()
                    });
                Arc::new(
                    trouve_agents::cursor::CursorBackend::new(id, command, api_key)
                        .with_catalog(catalog.clone()),
                )
            }
            "claude-cli" => Arc::new(
                trouve_agents::claude::ClaudeBackend::new(id, command)
                    .with_catalog(catalog.clone()),
            ),
            _ => continue,
        };
        backends.insert(id.clone(), backend);
    }
    backends
}

impl Engine {
    pub(crate) async fn acquire_planned_turn_setup(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.turn_scheduler.acquire_planned_setup(cancel).await
    }
}

impl Engine {
    pub fn new(store: Store, data_dir: PathBuf, config: &Config) -> Self {
        // Desktop launchers commonly inherit a reduced PATH. Capture the
        // user's login-shell PATH during server startup so the first provider
        // or MCP launch never pays shell initialization latency.
        let _ = trouve_agents::process_env::effective_path();
        let secrets: Arc<dyn trouve_providers::secrets::SecretStore> =
            Arc::from(trouve_providers::secrets::default_store(&data_dir));
        let model_catalog =
            Arc::new(trouve_providers::models_dev::ModelsDevCatalog::for_data_dir(&data_dir));
        let mut providers = build_all_providers(config, &secrets, &model_catalog);
        let backends = build_all_backends(config, &secrets, &data_dir, &model_catalog);
        let mcp_logs = crate::mcp::McpLogStore::default();
        let config_dir = dirs::config_dir().map(|d| d.join("trouve"));
        // The built-in "local" provider (managed llama-server). Registered
        // unless the user disabled local models — it lists no models until
        // a GGUF is downloaded — and seeded as injected so config-driven
        // reloads keep it.
        // Construction reaps llama-servers leaked by a crashed previous run
        // (they hold VRAM and would starve this run's model loads).
        let local_manager = Arc::new(crate::local::LlamaManager::new(&data_dir));
        let title_model = Arc::new(crate::title_model::TitleModelManager::new(
            data_dir.clone(),
            config.title_model_load_behavior.unwrap_or_default(),
            config.title_model_resource_policy.unwrap_or_default(),
            config
                .derive_branch_name_from_session_title
                .unwrap_or(false),
            &local_manager,
            store.clone(),
        ));
        local_manager.set_adaptive_title(Arc::downgrade(&title_model));
        let local_provider: Arc<dyn Provider> = Arc::new(crate::local::LocalProvider::new(
            data_dir.clone(),
            config_dir.clone(),
            local_manager.clone(),
        ));
        let mut injected_providers = HashMap::new();
        if config.local_enabled.unwrap_or(true) {
            providers.insert("local".into(), local_provider.clone());
            injected_providers.insert("local".to_string(), local_provider.clone());
        }
        Self {
            store,
            data_dir,
            config_dir,
            workspace_list_cache: Mutex::new(HashMap::new()),
            workspace_list_refresh_locks: Mutex::new(HashMap::new()),
            model_catalog,
            providers: RwLock::new(providers),
            injected_providers: Mutex::new(injected_providers),
            backends: RwLock::new(backends),
            injected_backends: Mutex::new(HashMap::new()),
            background_turn_intake: Mutex::new(Vec::new()),
            background_turn_intake_notify: Arc::new(tokio::sync::Notify::new()),
            executor: Arc::new(LocalToolExecutor::with_mcp_logs(mcp_logs.clone())),
            approvals: Arc::new(ApprovalHub::default()),
            questions: Arc::new(QuestionHub::default()),
            turn_scheduler: TurnScheduler::new(),
            automated_review_tool_budgets: Arc::new(AutomatedReviewToolBudgets::default()),
            session_locks: Mutex::new(HashMap::new()),
            session_create_locks: Mutex::new(HashMap::new()),
            tool_execution_locks: Mutex::new(HashMap::new()),
            session_pr_verification_locks: Mutex::new(HashMap::new()),
            session_pr_verification_wake: Arc::new(tokio::sync::Notify::new()),
            session_pr_verification_worker_started: AtomicBool::new(false),
            subagent_tree_locks: Mutex::new(HashMap::new()),
            active_threads: Mutex::new(std::collections::HashMap::new()),
            session_activity_publication: Mutex::new(()),
            prompt_queue_mutations: Mutex::new(()),
            deleting_sessions: Mutex::new(std::collections::HashSet::new()),
            turn_cancels: Mutex::new(std::collections::HashMap::new()),
            turn_steerers: Mutex::new(HashMap::new()),
            resume_after_cancel: Mutex::new(HashSet::new()),
            github_dashboard_caches: Mutex::new(HashMap::new()),
            github_pr_detail_cache: Mutex::new(GithubPrDetailCache::default()),
            github_dashboard_publication: Mutex::new(()),
            provider_locks: Mutex::new(HashMap::new()),
            persona_mutations: Arc::new(tokio::sync::Mutex::new(())),
            config: Mutex::new(config.clone()),
            // No write-back by default: only a caller that loaded `config`
            // from disk should enable persisting to that file (see
            // `with_config_file`). Defaulting to the real config path here
            // let test/embedded engines built from synthetic configs
            // clobber the user's config.toml on any provider change.
            config_file: None,
            title_model_behavior_transition: tokio::sync::Mutex::new(()),
            global_defaults: RwLock::new(GlobalDefaults {
                model: config
                    .default_model
                    .clone()
                    .unwrap_or_else(|| "openai/gpt-4.1-mini".into()),
                thinking_level: config.default_thinking_level.clone(),
                permission_mode: config.default_permission_mode.unwrap_or_default(),
            }),
            secrets,
            code_review: crate::review::CodeReviewRuntime::default(),
            logins: Mutex::new(HashMap::new()),
            cli_installs: Mutex::new(HashMap::new()),
            local_manager,
            title_model,
            title_model_generation: tokio::sync::Mutex::new(None),
            local_provider,
            local_downloads: Mutex::new(HashMap::new()),
            hardware: std::sync::OnceLock::new(),
            compaction_warnings: Mutex::new(std::collections::HashSet::new()),
            cli_latest: Mutex::new(HashMap::new()),
            base_url: RwLock::new(None),
            bridge_token: RwLock::new(None),
            bridge_tickets: Mutex::new(HashMap::new()),
            bridged_tool_owners: BridgedToolOwnerRouter::default(),
            index_hooks: false,
            mcp_logs,
            terminals: Arc::new(crate::terminal::TerminalManager::default()),
            online: std::sync::atomic::AtomicBool::new(true),
            connectivity_probe: None,
        }
    }

    /// Enable search-index lifecycle hooks: warm the index when a session is
    /// created (the in-process analogue of the agent plugins' SessionStart
    /// hook) and sweep the shared store when one is archived or deleted.
    pub fn with_index_hooks(mut self) -> Self {
        self.index_hooks = true;
        self
    }

    /// Remove immutable checkpoint refs that have no durable row. Server
    /// startup calls this before accepting requests so a crash between Git
    /// anchoring and SQLite commit cannot leak objects indefinitely.
    pub async fn reconcile_checkpoint_refs(&self) {
        let sessions = match self.store.list_sessions(None) {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "failed to list sessions for checkpoint reconciliation");
                return;
            }
        };
        for session in sessions {
            let live = match self.store.checkpoint_ids(&session.id) {
                Ok(live) => live,
                Err(error) => {
                    tracing::warn!(
                        session_id = %session.id,
                        %error,
                        "failed to load durable checkpoints for reconciliation"
                    );
                    continue;
                }
            };
            if let Err(error) = self
                .executor
                .reconcile_checkpoint_worktree_refs(
                    Path::new(&session.worktree_path),
                    &session.id,
                    &live,
                )
                .await
            {
                tracing::warn!(
                    session_id = %session.id,
                    %error,
                    "failed to reconcile checkpoint refs at startup"
                );
            }
        }
    }

    /// Retry every durable artifact-deletion intent before the server starts
    /// accepting requests. Cleanup operations are idempotent, so a crash
    /// after filesystem success but before row retirement is harmless.
    pub async fn retry_artifact_cleanup_jobs(&self) {
        loop {
            let job = match self.store.claim_next_artifact_cleanup_job() {
                Ok(Some(job)) => job,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "failed to claim durable artifact cleanup job");
                    break;
                }
            };
            execute_artifact_cleanup_job(
                self.store.clone(),
                self.executor.clone(),
                self.data_dir.join("attachments"),
                self.data_dir.join("worktrees"),
                job,
                self.index_hooks,
            )
            .await;
        }
    }

    /// Finish durable persona-file deletions left by an interrupted request.
    /// The intent keeps repository references untouched until the executor
    /// confirms the file is gone; a missing file is success only on replay.
    pub async fn retry_persona_deletions(&self) {
        let Some(config_dir) = self.config_dir.as_deref() else {
            return;
        };
        loop {
            let _mutation = self.persona_mutations.lock().await;
            let claim = match self.store.claim_next_persona_deletion() {
                Ok(Some(claim)) => claim,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(%error, "failed to claim durable persona deletion intent");
                    break;
                }
            };
            // A replacement may have consumed the intent while this worker
            // waited for the mutation lane.
            match self.store.persona_deletion_pending(&claim.id) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(persona_id = %claim.id, %error, "failed to recheck persona deletion intent");
                    if let Err(store_error) = self.store.fail_claimed_persona_deletion(&claim) {
                        tracing::warn!(persona_id = %claim.id, %store_error, "failed to back off persona deletion after recheck error");
                    }
                    break;
                }
            }
            let result = self
                .executor
                .delete_persona_file(config_dir, &claim.id, true)
                .await
                .map_err(anyhow::Error::msg)
                .and_then(|()| self.store.complete_claimed_persona_deletion(&claim));
            if let Err(error) = result {
                if let Err(store_error) = self.store.fail_claimed_persona_deletion(&claim) {
                    tracing::error!(
                        persona_id = %claim.id,
                        %error,
                        %store_error,
                        "persona deletion failed and its durable retry could not be updated"
                    );
                } else if claim.attempts == 0 {
                    tracing::warn!(persona_id = %claim.id, %error, "persona deletion retry failed");
                } else {
                    tracing::debug!(persona_id = %claim.id, %error, "persona deletion retry failed");
                }
            }
        }
    }

    /// Keep retrying failed cleanup intents while the process remains alive.
    /// Atomic store claims prevent this worker from overlapping immediate
    /// cleanup or another server instance sharing the database.
    pub fn start_artifact_cleanup_worker(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                engine.retry_artifact_cleanup_jobs().await;
                engine.retry_persona_deletions().await;
            }
        });
    }

    fn schedule_artifact_cleanup(&self, job: ArtifactCleanupJob) {
        let store = self.store.clone();
        let executor = self.executor.clone();
        let attachment_root = self.data_dir.join("attachments");
        let managed_worktree_root = self.data_dir.join("worktrees");
        let index_hooks = self.index_hooks;
        let cleanup = async move {
            match store.claim_artifact_cleanup_job(&job.id) {
                Ok(Some(job)) => {
                    execute_artifact_cleanup_job(
                        store,
                        executor,
                        attachment_root,
                        managed_worktree_root,
                        job,
                        index_hooks,
                    )
                    .await;
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    job_id = %job.id,
                    %error,
                    "failed to claim immediate artifact cleanup"
                ),
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(cleanup);
            }
            Err(no_runtime) => {
                // Core's synchronous APIs are also exercised by embedders and
                // tests outside Tokio. Complete the immediate best-effort
                // cleanup there instead of panicking; the durable job remains
                // available to the startup worker if runtime construction
                // itself fails.
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(cleanup),
                    Err(error) => tracing::warn!(
                        %no_runtime,
                        %error,
                        "could not start a runtime for immediate artifact cleanup"
                    ),
                }
            }
        }
    }

    /// Enable internet-reachability monitoring with the given probe (the
    /// server binary passes [`crate::connectivity::system_probe`]; tests can
    /// inject a scripted one). Without a probe the engine always reports
    /// online and never touches the network.
    pub fn with_connectivity_probe(mut self, probe: crate::connectivity::Probe) -> Self {
        self.connectivity_probe = Some(probe);
        self
    }

    /// Whether the server can currently reach the internet.
    pub fn is_online(&self) -> bool {
        self.online.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Force the connectivity state (tests/embedders). Emits the
    /// `server.connectivity_changed` event on an actual transition, exactly
    /// like the probe-driven monitor.
    pub fn set_online(&self, online: bool) {
        self.transition_connectivity(online);
    }

    fn transition_connectivity(&self, online: bool) {
        let was = self
            .online
            .swap(online, std::sync::atomic::Ordering::Relaxed);
        if was == online {
            return;
        }
        if online {
            tracing::info!("connectivity restored");
        } else {
            tracing::warn!("connectivity lost: model vendors are unreachable");
        }
        let _ = self
            .store
            .append_event(Scope::Server, Event::ConnectivityChanged { online });
    }

    /// Run the first connectivity probe (no-op without one). Called before
    /// the server starts accepting requests so an offline start never serves
    /// a model list it will immediately retract.
    pub async fn init_connectivity(&self) {
        if let Some(probe) = self.connectivity_probe.clone() {
            let online = probe().await;
            if online && let Err(error) = self.model_catalog.refresh_if_stale().await {
                tracing::debug!("models.dev refresh failed; using cached snapshot: {error:#}");
            }
            self.transition_connectivity(online);
        }
    }

    /// Poll the connectivity probe for the lifetime of the server: slowly
    /// while online (going offline is rarely urgent), quickly while offline
    /// (clients unblock prompt entry off the recovery event). No-op without
    /// a probe.
    /// Listen for vendor-autonomous turn signals from agent backends and
    /// dispatch attach turns so that activity is persisted and rendered
    /// live instead of buffering silently inside the adapter. Without this,
    /// a vendor harness that wakes itself (e.g. Claude Code monitors and
    /// scheduled tasks) produces output no one is reading: the adapter now
    /// buffers it, and this listener turns it into an ordinary turn.
    pub fn start_background_turn_listener(self: &Arc<Self>) {
        self.intake_background_turn_signals();
        // The pump and its forwarders hold only a Weak engine reference and
        // exit when it no longer upgrades: a forever-parked task must not be
        // what keeps the Engine (and through it every backend) alive.
        let weak = Arc::downgrade(self);
        let notify = Arc::clone(&self.background_turn_intake_notify);
        tokio::spawn(async move {
            loop {
                {
                    let Some(engine) = weak.upgrade() else {
                        return;
                    };
                    let pending: Vec<(String, tokio::sync::mpsc::Receiver<String>)> = {
                        let mut intake = engine.background_turn_intake.lock().unwrap();
                        intake.drain(..).collect()
                    };
                    for (backend_id, mut signals) in pending {
                        let weak = weak.clone();
                        tokio::spawn(async move {
                            // The forwarder ends when its backend (the
                            // sender) is dropped by a registry reload; the
                            // replacement backend's receiver arrives through
                            // the intake.
                            while let Some(thread_id) = signals.recv().await {
                                let Some(engine) = weak.upgrade() else {
                                    return;
                                };
                                engine
                                    .dispatch_background_attach_turn_with_retry(
                                        &thread_id,
                                        &backend_id,
                                    )
                                    .await;
                            }
                        });
                    }
                }
                notify.notified().await;
            }
        });
    }

    /// Dispatch with one bounded retry: a notification is consumed from the
    /// signal channel when received, so a transiently failing dispatch (a
    /// busy store, a mid-write race) must not silently strand the buffered
    /// autonomous turn until some later boundary happens to re-announce it.
    async fn dispatch_background_attach_turn_with_retry(
        self: &Arc<Self>,
        thread_id: &str,
        backend_id: &str,
    ) {
        for attempt in 0..2_u8 {
            match self.dispatch_background_attach_turn(thread_id, backend_id) {
                Ok(()) => return,
                // The thread is gone; the buffered turns can never attach.
                // Tell the backend to abandon them so they stop pinning the
                // process in its pool.
                Err(EngineError::NotFound(_)) => {
                    let backend = self.backends.read().unwrap().get(backend_id).cloned();
                    if let Some(backend) = backend {
                        backend.abandon_background_turns(thread_id).await;
                    }
                    return;
                }
                Err(error) if attempt == 0 => {
                    tracing::debug!(
                        %thread_id,
                        backend = %backend_id,
                        %error,
                        "background attach-turn dispatch failed; retrying once"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                Err(error) => {
                    tracing::warn!(
                        %thread_id,
                        backend = %backend_id,
                        %error,
                        "background attach-turn dispatch failed"
                    );
                }
            }
        }
    }

    /// Collect every registered backend's background-turn signal receiver
    /// into the intake. `take_background_turn_signals` yields a receiver at
    /// most once per backend instance, so calling this after a registry
    /// reload arms exactly the new instances.
    fn intake_background_turn_signals(&self) {
        let backends: Vec<(String, Arc<dyn AgentBackend>)> = {
            let map = self.backends.read().unwrap();
            map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        let mut taken = Vec::new();
        for (backend_id, backend) in backends {
            if let Some(signals) = backend.take_background_turn_signals() {
                taken.push((backend_id, signals));
            }
        }
        if taken.is_empty() {
            return;
        }
        self.background_turn_intake.lock().unwrap().extend(taken);
        self.background_turn_intake_notify.notify_one();
    }

    /// Queue one attach turn for a vendor-autonomous turn the backend
    /// reported on `thread_id`. The prompt is a fixed marker: the backend
    /// recognizes it via `BackendTurn::attach_background` and consumes the
    /// autonomous turn's events instead of prompting the model again.
    fn dispatch_background_attach_turn(
        self: &Arc<Self>,
        thread_id: &str,
        signaling_backend_id: &str,
    ) -> Result<(), EngineError> {
        // Review threads run under strict budgets and their vendor sessions
        // have no monitors; skip them defensively.
        if self
            .store
            .is_code_review_thread(thread_id)
            .map_err(EngineError::Internal)?
        {
            return Ok(());
        }
        // One queued attach drains the backend's buffered turns; backlog
        // re-announcements for the same buffer coalesce into it instead of
        // queueing surplus attaches that would surface as empty turns.
        if self
            .store
            .has_queued_background_prompt(thread_id)
            .map_err(EngineError::Internal)?
        {
            return Ok(());
        }
        // The thread may have switched models between the signal and this
        // dispatch. Attaching would then queue the marker prompt for a
        // backend with no pending autonomous turn — worst case a backend
        // that treats it as a literal prompt — so confirm the thread still
        // resolves to the signaling backend.
        let thread = self.get_thread(thread_id)?;
        match self.backend_for(&thread.model) {
            Some((backend_id, _, _)) if backend_id == signaling_backend_id => {}
            resolved => {
                tracing::debug!(
                    %thread_id,
                    signaling_backend = %signaling_backend_id,
                    resolved_backend = resolved.map(|(id, _, _)| id).unwrap_or_default(),
                    "skipping background attach: thread no longer resolves to the signaling backend"
                );
                return Ok(());
            }
        }
        self.send_message_inner(
            thread_id,
            BACKGROUND_ATTACH_PROMPT.to_string(),
            Vec::new(),
            true,
            true,
            true,
        )
        .map(|_| ())
    }

    pub fn start_connectivity_monitor(self: &Arc<Self>) {
        let Some(probe) = self.connectivity_probe.clone() else {
            return;
        };
        let engine = self.clone();
        tokio::spawn(async move {
            loop {
                let interval = if engine.is_online() {
                    crate::connectivity::ONLINE_POLL
                } else {
                    crate::connectivity::OFFLINE_POLL
                };
                tokio::time::sleep(interval).await;
                let online = probe().await;
                let recovering = online && !engine.is_online();
                if recovering && let Err(error) = engine.model_catalog.refresh_if_stale().await {
                    tracing::debug!("models.dev refresh failed; using cached snapshot: {error:#}");
                }
                engine.transition_connectivity(online);
            }
        });
    }

    /// Record the server's reachable base URL (enables the default MCP tool
    /// bridge for supported vendor backends).
    pub fn set_base_url(&self, url: &str) {
        *self.base_url.write().unwrap() = Some(url.trim_end_matches('/').to_string());
    }

    /// Set the server-generated credential vendor children must present to
    /// the internal MCP bridge. `None` keeps in-process open test routers
    /// backwards-compatible.
    pub fn set_bridge_token(&self, token: Option<String>) {
        *self.bridge_token.write().unwrap() = token;
    }

    /// Validate one engine-issued MCP bridge capability. The process-wide
    /// bridge token authenticates the caller to the internal route; this
    /// active-turn ticket additionally binds the durable root path and exact
    /// tool/approval surface so query-string tampering cannot widen it.
    pub fn validate_bridge_ticket(
        &self,
        ticket: &str,
        root_thread_id: &str,
        bridge_tools: bool,
        serve_approval: bool,
    ) -> Option<BridgeTicketClaims> {
        if !self
            .turn_cancels
            .lock()
            .unwrap()
            .contains_key(root_thread_id)
        {
            return None;
        }
        let tickets = self.bridge_tickets.lock().unwrap();
        let ticket = tickets.get(ticket)?;
        (ticket.root_thread_id == root_thread_id
            && ticket.claims.bridge_tools == bridge_tools
            && ticket.claims.serve_approval == serve_approval)
            .then_some(ticket.claims)
    }

    fn bridge_ticket_for(&self, root_thread_id: &str, claims: BridgeTicketClaims) -> String {
        let mut tickets = self.bridge_tickets.lock().unwrap();
        if let Some(ticket) = tickets.iter().find_map(|(ticket, active)| {
            (active.root_thread_id == root_thread_id && active.claims == claims)
                .then(|| ticket.clone())
        }) {
            return ticket;
        }

        // A thread can change provider or bridge policy between turns. Never
        // leave its previous capability surface reusable when that happens.
        tickets.retain(|_, ticket| ticket.root_thread_id != root_thread_id);
        let ticket = new_id("bridge");
        tickets.insert(
            ticket.clone(),
            ActiveBridgeTicket {
                root_thread_id: root_thread_id.to_string(),
                claims,
            },
        );
        ticket
    }

    fn revoke_bridge_tickets(&self, root_thread_id: &str) {
        self.bridge_tickets
            .lock()
            .unwrap()
            .retain(|_, ticket| ticket.root_thread_id != root_thread_id);
    }

    /// Swap the tool executor (cloud isolation hook, ADR 0004).
    pub fn with_executor(mut self, executor: Arc<dyn ToolExecutor>) -> Self {
        self.executor = executor;
        self
    }

    /// Register (or replace) a provider instance under an id. Survives
    /// config-driven registry reloads.
    pub fn with_provider(self, id: &str, provider: Arc<dyn Provider>) -> Self {
        self.injected_providers
            .lock()
            .unwrap()
            .insert(id.to_string(), provider.clone());
        self.providers
            .write()
            .unwrap()
            .insert(id.to_string(), provider);
        self
    }

    /// Register (or replace) an agent backend instance under an id. Survives
    /// config-driven registry reloads (tests, embedders).
    pub fn with_backend(self, id: &str, backend: Arc<dyn AgentBackend>) -> Self {
        self.injected_backends
            .lock()
            .unwrap()
            .insert(id.to_string(), backend.clone());
        self.backends
            .write()
            .unwrap()
            .insert(id.to_string(), backend);
        self
    }

    /// Override the default model for new threads.
    pub fn with_default_model(self, model: &str) -> Self {
        self.global_defaults.write().unwrap().model = model.to_string();
        self
    }

    /// Override the global default thinking level for new threads.
    pub fn with_default_thinking_level(self, level: Option<&str>) -> Self {
        self.global_defaults.write().unwrap().thinking_level = level.map(String::from);
        self
    }

    /// Override the config dir used for mode/AGENTS.md discovery (tests).
    pub fn with_config_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.config_dir = dir;
        self
    }

    /// Override (or disable, with `None`) where provider config changes are
    /// written.
    pub fn with_config_file(mut self, path: Option<PathBuf>) -> Self {
        self.config_file = path;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.providers.read().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Instant model snapshot for first paint. Catalog-covered providers and
    /// backends use their offline-safe static rosters, while explicit adapters
    /// may expose a cached live result. Backends without credentials are
    /// skipped entirely — their models cannot run, so listing them only
    /// clutters the picker.
    ///
    /// While the server is offline, only models that can actually run are
    /// listed: the built-in local provider and loopback endpoints (Ollama
    /// etc.). Remote providers and vendor backends are dropped instead of
    /// degrading to static/fallback catalogs of models every turn would fail
    /// on. Live account and vendor-CLI availability is resolved separately by
    /// refresh_models so first paint never waits for network or CLI startup.
    pub async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        let online = self.is_online();
        let offline_capable = if online {
            std::collections::HashSet::new()
        } else {
            self.offline_capable_provider_ids()
        };
        let providers: Vec<_> = self
            .providers
            .read()
            .unwrap()
            .iter()
            .filter(|(id, _)| online || offline_capable.contains(id.as_str()))
            .map(|(_, p)| p.clone())
            .collect();
        let mut models: Vec<_> = providers
            .iter()
            .flat_map(|provider| provider.models())
            .collect();
        let ready: Vec<_> = if online {
            self.backends
                .read()
                .unwrap()
                .values()
                .filter(|b| {
                    let status = b.status();
                    status.installed && status.has_credentials
                })
                .cloned()
                .collect()
        } else {
            Vec::new() // vendor backends all need their cloud
        };
        models.extend(ready.iter().flat_map(|backend| backend.models()));
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    /// Resolve live account-visible and vendor-CLI model availability. Clients
    /// call this after painting list_models, then replace the static snapshot
    /// when this richer result arrives.
    pub async fn refresh_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        let online = self.is_online();
        if online
            && self.connectivity_probe.is_some()
            && let Err(error) = self.model_catalog.refresh_if_stale().await
        {
            tracing::debug!("models.dev refresh failed; using cached snapshot: {error:#}");
        }
        let offline_capable = if online {
            std::collections::HashSet::new()
        } else {
            self.offline_capable_provider_ids()
        };
        let providers: Vec<_> = self
            .providers
            .read()
            .unwrap()
            .iter()
            .filter(|(id, _)| online || offline_capable.contains(id.as_str()))
            .map(|(_, provider)| provider.clone())
            .collect();
        let provider_lists =
            futures::future::join_all(providers.iter().map(|provider| provider.list_models()))
                .await;
        let mut models: Vec<_> = provider_lists.into_iter().flatten().collect();
        let ready: Vec<_> = if online {
            self.backends
                .read()
                .unwrap()
                .values()
                .filter(|backend| {
                    let status = backend.status();
                    status.installed && status.has_credentials
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let listings = futures::future::join_all(ready.iter().map(|b| b.list_models())).await;
        models.extend(listings.into_iter().flatten());
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    /// Provider ids that keep working without internet: the built-in local
    /// provider plus configured loopback endpoints.
    fn offline_capable_provider_ids(&self) -> std::collections::HashSet<String> {
        let mut ids: std::collections::HashSet<String> = ["local".to_string()].into();
        let config = self.config.lock().unwrap();
        for (id, pc) in &config.providers {
            if pc.base_url.as_deref().is_some_and(is_loopback_base_url) {
                ids.insert(id.clone());
            }
        }
        ids
    }

    // --- provider configuration ----------------------------------------------

    /// Well-known provider presets for one-click setup in clients.
    pub async fn known_providers(&self) -> Vec<trouve_protocol::KnownProvider> {
        if self.is_online()
            && self.connectivity_probe.is_some()
            && let Err(error) = self.model_catalog.refresh_if_stale().await
        {
            tracing::debug!("models.dev refresh failed; using cached snapshot: {error:#}");
        }
        trouve_providers::catalog::known_providers(&self.model_catalog)
    }

    /// Configured providers (secrets elided) plus the default model.
    pub fn list_providers(&self) -> ProvidersResponse {
        let config = self.config.lock().unwrap();
        let registry = self.providers.read().unwrap();
        let mut infos: Vec<ProviderInfo> = config
            .providers
            .iter()
            .map(|(id, pc)| {
                let auth = provider_auth_kind(pc);
                let has_credentials = self.provider_has_credentials(id, pc, &auth, &registry);
                ProviderInfo {
                    id: id.clone(),
                    kind: pc.kind.clone(),
                    base_url: pc.base_url.clone(),
                    settings: pc.settings.clone(),
                    has_credentials,
                    category: trouve_providers::catalog::provider_category(
                        id,
                        &auth,
                        pc.base_url.as_deref(),
                    ),
                    auth,
                    experimental: false,
                }
            })
            .collect();
        // Zero-config providers (env keys) that aren't in the config file.
        for id in registry.keys() {
            if !config.providers.contains_key(id) {
                let local = id == "local";
                infos.push(ProviderInfo {
                    id: id.clone(),
                    kind: if id == "anthropic" {
                        "anthropic".into()
                    } else {
                        "openai-compat".into()
                    },
                    base_url: None,
                    settings: Default::default(),
                    has_credentials: true,
                    auth: if local { "none" } else { "api-key" }.into(),
                    category: if local { "local" } else { "api" }.into(),
                    experimental: false,
                });
            }
        }
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        let defaults = self.global_defaults.read().unwrap().clone();
        ProvidersResponse {
            providers: infos,
            default_model: defaults.model,
            default_thinking_level: defaults.thinking_level,
            default_permission_mode: defaults.permission_mode,
        }
    }

    /// Best-effort credential presence for one configured provider.
    fn provider_has_credentials(
        &self,
        id: &str,
        pc: &ProviderConfig,
        auth: &str,
        registry: &HashMap<String, Arc<dyn Provider>>,
    ) -> bool {
        match auth {
            // Vendor CLI holds the auth; adapters do cheap fs checks.
            "cli" => self
                .backends
                .read()
                .unwrap()
                .get(id)
                .map(|b| {
                    let s = b.status();
                    s.installed && s.has_credentials
                })
                .unwrap_or(false),
            // OAuth providers build lazily, so registry membership alone
            // doesn't prove credentials — check for stored tokens.
            "oauth" => self
                .secrets
                .get(&trouve_providers::secrets::oauth_secret(id))
                .ok()
                .flatten()
                .is_some(),
            // Key-authenticated agent backend (cursor-api): not in the
            // provider registry, so check the key channels directly.
            _ if is_backend_kind(&pc.kind) => {
                pc.api_key.is_some()
                    || pc
                        .api_key_env
                        .as_ref()
                        .map(|v| std::env::var(v).is_ok())
                        .unwrap_or(false)
                    || self
                        .secrets
                        .get(&trouve_providers::secrets::api_key_secret(id))
                        .ok()
                        .flatten()
                        .is_some()
            }
            _ => registry.contains_key(id),
        }
    }

    /// Create or update a provider. The API key (when present) goes to the
    /// secret store; the config file only holds non-secret settings.
    pub fn upsert_provider(
        &self,
        id: &str,
        req: &UpsertProviderRequest,
    ) -> Result<ProviderInfo, EngineError> {
        if !matches!(
            req.kind.as_str(),
            "openai-compat"
                | "anthropic"
                | "azure-openai"
                | "amazon-bedrock"
                | "google-vertex"
                | "google-vertex-anthropic"
        ) && !is_cli_auth_kind(&req.kind)
        {
            return Err(EngineError::BadRequest(format!(
                "unknown provider kind {:?} (expected openai-compat, anthropic, \
                 azure-openai, amazon-bedrock, google-vertex, \
                 google-vertex-anthropic, codex-app-server, \
                 cursor-cli, or claude-cli)",
                req.kind
            )));
        }
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(EngineError::BadRequest(
                "provider id must be non-empty ascii alphanumeric/dashes".into(),
            ));
        }
        let provider_lock = self.provider_lock(id);
        let _provider_guard = provider_lock.lock().unwrap();
        if let Some(key) = req.api_key.as_deref().filter(|k| !k.is_empty()) {
            self.secrets
                .set(&trouve_providers::secrets::api_key_secret(id), key)
                .map_err(EngineError::Internal)?;
        }
        for (name, value) in req
            .secret_values
            .iter()
            .filter(|(_, value)| !value.is_empty())
        {
            self.secrets
                .set(&trouve_providers::secrets::provider_secret(id, name), value)
                .map_err(EngineError::Internal)?;
        }
        {
            let mut config = self.config.lock().unwrap();
            let entry = config.providers.entry(id.to_string()).or_default();
            entry.kind = req.kind.clone();
            if let Some(base_url) = req.base_url.clone().filter(|url| !url.is_empty()) {
                entry.base_url = Some(base_url);
            }
            if !req.settings.is_empty() {
                entry.settings = req.settings.clone();
            }
            for name in req.secret_values.keys() {
                if !entry.secret_names.contains(name) {
                    entry.secret_names.push(name.clone());
                }
            }
            let preset = trouve_providers::catalog::known_providers(&self.model_catalog)
                .into_iter()
                .find(|known| known.id == id && known.kind == req.kind);
            if let Some(preset) = preset {
                if entry.api_key_env.is_none() {
                    entry.api_key_env = preset.api_key_env;
                }
                if entry.base_url.is_none() {
                    entry.base_url = preset.base_url;
                }
                if !req.headers.is_empty() {
                    entry.headers = req.headers.clone();
                } else if entry.headers.is_empty() {
                    entry.headers = preset.headers;
                }
                if !req.query_params.is_empty() {
                    entry.query_params = req.query_params.clone();
                } else if entry.query_params.is_empty() {
                    entry.query_params = preset.query_params;
                }
            } else {
                if !req.headers.is_empty() {
                    entry.headers = req.headers.clone();
                }
                if !req.query_params.is_empty() {
                    entry.query_params = req.query_params.clone();
                }
            }
            self.persist_config(&config);
        }
        self.reload_providers();
        let config = self.config.lock().unwrap();
        let registry = self.providers.read().unwrap();
        let pc = config.providers.get(id).cloned().unwrap_or_default();
        let auth = provider_auth_kind(&pc);
        let has_credentials = self.provider_has_credentials(id, &pc, &auth, &registry);
        Ok(ProviderInfo {
            id: id.to_string(),
            kind: req.kind.clone(),
            base_url: pc.base_url.clone(),
            settings: pc.settings.clone(),
            has_credentials,
            category: trouve_providers::catalog::provider_category(
                id,
                &auth,
                req.base_url.as_deref(),
            ),
            auth,
            experimental: false,
        })
    }

    /// Remove a provider from the config and its stored API key.
    pub fn delete_provider(&self, id: &str) -> Result<(), EngineError> {
        let provider_lock = self.provider_lock(id);
        let _provider_guard = provider_lock.lock().unwrap();
        let secret_names = {
            let mut config = self.config.lock().unwrap();
            let removed = config
                .providers
                .remove(id)
                .ok_or_else(|| EngineError::NotFound(format!("provider {id}")))?;
            self.persist_config(&config);
            removed.secret_names
        };
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::api_key_secret(id));
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::oauth_secret(id));
        for name in secret_names {
            let _ = self
                .secrets
                .delete(&trouve_providers::secrets::provider_secret(id, &name));
        }
        self.reload_providers();
        Ok(())
    }

    fn provider_lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.provider_locks
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    // --- OAuth login (subscription providers) ---------------------------------

    /// Start an OAuth login for a configured provider. Returns what the user
    /// must do (open a URL, possibly enter a code); the exchange runs in the
    /// background and `login_status` reports how it went.
    pub async fn start_login(
        self: &Arc<Self>,
        id: &str,
    ) -> Result<trouve_protocol::LoginStarted, EngineError> {
        use trouve_providers::auth as oauth_flow;

        // Vendor-CLI logins (subscription backends) go through the vendor's
        // own flow; everything else uses our generic OAuth machinery.
        let cli_auth = {
            let config = self.config.lock().unwrap();
            config
                .providers
                .get(id)
                .is_some_and(|pc| is_cli_auth_kind(&pc.kind))
        };
        if cli_auth {
            return self.start_cli_login(id).await;
        }

        // "Sign in with GitHub" (Integrations, not a model provider): id
        // "github" is github.com, "github:<host>" a GitHub Enterprise
        // instance. Device flow against that host; the token lands in the
        // oauth secret github_token() reads, because the login id and the
        // host's secret id are the same string.
        let github_host = if id == "github" {
            Some(crate::github::GITHUB_COM.to_string())
        } else {
            id.strip_prefix("github:").map(str::to_string)
        };
        let oauth = if let Some(host) = github_host {
            let client_id = self
                .github_hosts()
                .into_iter()
                .find(|(h, _)| *h == host)
                .ok_or_else(|| EngineError::NotFound(format!("GitHub host {host}")))?
                .1
                .ok_or_else(|| {
                    EngineError::BadRequest(format!(
                        "GitHub OAuth is not configured for {host}: set a client id of an \
                         OAuth app (device flow enabled) on that host"
                    ))
                })?;
            crate::github::oauth_config(&host, &client_id)
        } else {
            let config = self.config.lock().unwrap();
            config
                .providers
                .get(id)
                .and_then(|pc| pc.oauth.clone())
                .ok_or_else(|| {
                    EngineError::BadRequest(format!("provider {id} has no OAuth configuration"))
                })?
        };
        // A login is already in flight (the user may have closed the
        // browser tab): re-present the same instructions — the URL/code
        // stay valid while the flow waits — instead of refusing.
        if let Some(LoginState::Pending { started, .. }) = self.logins.lock().unwrap().get(id) {
            return Ok(started.clone());
        }

        if oauth.device_authorization_url.is_some() {
            // RFC 8628 device flow: show the code, poll in the background.
            let device = oauth_flow::device_authorize(&oauth)
                .await
                .map_err(|e| EngineError::BadRequest(e.to_string()))?;
            let started = trouve_protocol::LoginStarted {
                verification_url: device
                    .verification_uri_complete
                    .clone()
                    .unwrap_or_else(|| device.verification_uri.clone()),
                user_code: Some(device.user_code.clone()),
            };
            self.logins.lock().unwrap().insert(
                id.to_string(),
                LoginState::Pending {
                    started: started.clone(),
                    callback_sender: None,
                },
            );
            let engine = self.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                let result = oauth_flow::device_poll(&oauth, &device).await;
                engine.finish_login(&id, result);
            });
            Ok(started)
        } else if oauth.authorization_url.is_some() {
            // PKCE browser flow: we listen on localhost for the redirect.
            let listener =
                tokio::net::TcpListener::bind(("127.0.0.1", oauth.redirect_port.unwrap_or(0)))
                    .await
                    .map_err(|e| {
                        EngineError::BadRequest(format!("cannot bind redirect port: {e}"))
                    })?;
            let redirect_uri = format!(
                "http://localhost:{}{}",
                listener.local_addr().map(|a| a.port()).unwrap_or_default(),
                oauth.redirect_path.as_deref().unwrap_or("/callback")
            );
            let challenge = oauth_flow::pkce_challenge();
            let state = uuid::Uuid::new_v4().simple().to_string();
            let url = oauth_flow::pkce_authorize_url(&oauth, &challenge, &redirect_uri, &state)
                .map_err(|e| EngineError::BadRequest(e.to_string()))?;
            let started = trouve_protocol::LoginStarted {
                verification_url: url,
                user_code: None,
            };
            self.logins.lock().unwrap().insert(
                id.to_string(),
                LoginState::Pending {
                    started: started.clone(),
                    callback_sender: None,
                },
            );
            let engine = self.clone();
            let id = id.to_string();
            tokio::spawn(async move {
                let result = async {
                    let code = tokio::time::timeout(
                        std::time::Duration::from_secs(600),
                        oauth_flow::pkce_wait_for_code(listener, &state),
                    )
                    .await
                    .map_err(|_| {
                        trouve_providers::ProviderError::Auth("login timed out".into())
                    })??;
                    oauth_flow::pkce_exchange(&oauth, &code, &challenge.verifier, &redirect_uri)
                        .await
                }
                .await;
                engine.finish_login(&id, result);
            });
            Ok(started)
        } else {
            Err(EngineError::BadRequest(format!(
                "provider {id} OAuth config has neither device_authorization_url \
                 nor authorization_url"
            )))
        }
    }

    /// Login for CLI-auth providers: run the vendor CLI's own login flow and
    /// surface its verification URL; `login_status` reports the outcome.
    async fn start_cli_login(
        self: &Arc<Self>,
        id: &str,
    ) -> Result<trouve_protocol::LoginStarted, EngineError> {
        // The vendor CLI is still waiting on its verification URL (the
        // user may have closed the browser tab): hand the same URL back
        // so the client can reopen it, rather than refusing.
        if let Some(LoginState::Pending { started, .. }) = self.logins.lock().unwrap().get(id) {
            return Ok(started.clone());
        }
        let backend = self
            .backends
            .read()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| EngineError::NotFound(format!("provider {id}")))?;
        let login = backend
            .start_login()
            .await
            .map_err(|e| EngineError::BadRequest(e.to_string()))?;

        let trouve_agents::BackendLogin {
            verification_url,
            user_code,
            callback_sender,
            done,
        } = login;
        let started = trouve_protocol::LoginStarted {
            verification_url: verification_url.unwrap_or_default(),
            user_code,
        };
        self.logins.lock().unwrap().insert(
            id.to_string(),
            LoginState::Pending {
                started: started.clone(),
                callback_sender,
            },
        );
        let engine = self.clone();
        let id_owned = id.to_string();
        tokio::spawn(async move {
            let state = match done.await {
                Ok(()) => LoginState::Success,
                Err(e) => LoginState::Failed(e.to_string()),
            };
            engine
                .logins
                .lock()
                .unwrap()
                .insert(id_owned.clone(), state);
        });
        Ok(started)
    }

    // --- managed vendor CLIs ---------------------------------------------------

    /// Install state of every vendor CLI trouve can manage: the binary that
    /// would run (managed install beats PATH), its version, and whether the
    /// vendor serves something newer (best-effort network check, cached).
    pub async fn list_clis(&self) -> trouve_protocol::CliList {
        use trouve_agents::install as cli;

        let mut clis = Vec::new();
        for id in cli::ALL_CLIS {
            let explicit = {
                // An explicit per-provider `command` overrides resolution;
                // surface it so the UI doesn't claim "not installed".
                let config = self.config.lock().unwrap();
                config
                    .providers
                    .values()
                    .filter(|pc| cli_for_kind(&pc.kind) == Some(id))
                    .find_map(|pc| pc.command.clone())
            };
            let managed = cli::installed(&self.data_dir, id);
            let (source, path, installed_version) = if let Some(cmd) = explicit {
                let version = cli::binary_version(&cmd).await;
                ("path".to_string(), Some(cmd), version)
            } else if let Some(info) = managed {
                ("managed".into(), Some(info.bin), Some(info.version))
            } else if let Some(found) = cli::find_on_path(id.as_str()) {
                let path = found.to_string_lossy().into_owned();
                let version = cli::binary_version(&path).await;
                ("path".into(), Some(path), version)
            } else {
                ("none".into(), None, None)
            };

            let latest_version = self.cli_latest_version(id).await;
            let update_available = match (&installed_version, &latest_version) {
                (Some(have), Some(latest)) => !cli_version_matches(have, latest),
                (None, Some(_)) => true,
                _ => false,
            };
            clis.push(trouve_protocol::CliInfo {
                id: id.as_str().into(),
                display_name: id.display_name().into(),
                kinds: id.provider_kinds().iter().map(|s| s.to_string()).collect(),
                installed_version,
                source,
                path,
                latest_version,
                update_available,
            });
        }
        trouve_protocol::CliList { clis }
    }

    /// Latest vendor version for one CLI, cached for an hour; None when the
    /// lookup fails (offline is fine — the UI just can't offer updates).
    async fn cli_latest_version(&self, id: trouve_agents::install::CliId) -> Option<String> {
        const TTL: std::time::Duration = std::time::Duration::from_secs(3600);
        {
            let cache = self.cli_latest.lock().unwrap();
            if let Some((at, v)) = cache.get(id.as_str())
                && at.elapsed() < TTL
            {
                return v.clone();
            }
        }
        let fetched = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            trouve_agents::install::latest_version(id),
        )
        .await;
        let latest = match fetched {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                tracing::debug!("latest-version check for {} failed: {e}", id.as_str());
                None
            }
            Err(_) => None,
        };
        self.cli_latest.lock().unwrap().insert(
            id.as_str().into(),
            (std::time::Instant::now(), latest.clone()),
        );
        latest
    }

    /// Start downloading the newest build of a vendor CLI into trouve's
    /// managed directory. Progress is reported by `cli_install_status`; on
    /// success the backend registry reloads so new turns use the new binary.
    pub fn start_cli_install(self: &Arc<Self>, id: &str) -> Result<(), EngineError> {
        let cli = trouve_agents::install::CliId::parse(id)
            .ok_or_else(|| EngineError::NotFound(format!("cli {id}")))?;
        let progress = Arc::new(trouve_agents::install::Progress::default());
        {
            let mut installs = self.cli_installs.lock().unwrap();
            if matches!(installs.get(id), Some(CliInstallState::Pending { .. })) {
                return Err(EngineError::Conflict(format!(
                    "an install for {id} is already in progress"
                )));
            }
            installs.insert(
                id.to_string(),
                CliInstallState::Pending {
                    version: None,
                    progress: progress.clone(),
                },
            );
        }
        let engine = self.clone();
        let id_owned = id.to_string();
        tokio::spawn(async move {
            let result = async {
                let version = trouve_agents::install::latest_version(cli)
                    .await
                    .map_err(|e| e.to_string())?;
                engine.cli_installs.lock().unwrap().insert(
                    id_owned.clone(),
                    CliInstallState::Pending {
                        version: Some(version.clone()),
                        progress: progress.clone(),
                    },
                );
                match trouve_agents::install::install(&engine.data_dir, cli, &version, &progress)
                    .await
                {
                    Ok(_) => Ok(Some(version)),
                    Err(trouve_agents::install::InstallError::Cancelled) => Ok(None),
                    Err(e) => Err(e.to_string()),
                }
            }
            .await;
            let mut installs = engine.cli_installs.lock().unwrap();
            match result {
                Ok(Some(version)) => {
                    // The managed binary now exists; rebuild backends so it
                    // takes over from any PATH resolution.
                    engine.reload_providers();
                    engine.cli_latest.lock().unwrap().remove(id_owned.as_str());
                    installs.insert(id_owned, CliInstallState::Success(version));
                }
                // Cancelled: back to "none", like it never started.
                Ok(None) => {
                    installs.remove(&id_owned);
                }
                Err(e) => {
                    installs.insert(id_owned, CliInstallState::Failed(e));
                }
            }
        });
        Ok(())
    }

    /// Ask an in-flight install started with `start_cli_install` to stop.
    /// The task notices at its next chunk and clears the install state.
    pub fn cancel_cli_install(&self, id: &str) -> Result<(), EngineError> {
        match self.cli_installs.lock().unwrap().get(id) {
            Some(CliInstallState::Pending { progress, .. }) => {
                progress
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            _ => Err(EngineError::NotFound(format!(
                "no install for {id} is in progress"
            ))),
        }
    }

    /// Remove the managed install of a CLI (PATH installs are untouched).
    /// For llama-server the sidecar is stopped first.
    pub async fn uninstall_cli(&self, id: &str) -> Result<(), EngineError> {
        let cli = trouve_agents::install::CliId::parse(id)
            .ok_or_else(|| EngineError::NotFound(format!("cli {id}")))?;
        {
            let installs = self.cli_installs.lock().unwrap();
            if matches!(installs.get(id), Some(CliInstallState::Pending { .. })) {
                return Err(EngineError::Conflict(format!(
                    "an install for {id} is in progress — cancel it first"
                )));
            }
        }
        if cli == trouve_agents::install::CliId::LlamaServer {
            self.local_manager.stop().await;
            self.title_model.stop().await;
        }
        trouve_agents::install::uninstall(&self.data_dir, cli)
            .map_err(|e| EngineError::Internal(e.into()))?;
        // Drop any stale success/failed state so status reads "none", and
        // rebuild backends so they fall back to PATH resolution (or none).
        self.cli_installs.lock().unwrap().remove(id);
        self.reload_providers();
        Ok(())
    }

    /// Report the state of an install started with `start_cli_install`.
    pub fn cli_install_status(&self, id: &str) -> trouve_protocol::CliInstallStatus {
        match self.cli_installs.lock().unwrap().get(id) {
            None => trouve_protocol::CliInstallStatus {
                status: "none".into(),
                version: None,
                error: None,
                received_bytes: 0,
                total_bytes: 0,
            },
            Some(CliInstallState::Pending { version, progress }) => {
                use std::sync::atomic::Ordering::Relaxed;
                trouve_protocol::CliInstallStatus {
                    status: "pending".into(),
                    version: version.clone(),
                    error: None,
                    received_bytes: progress.received.load(Relaxed),
                    total_bytes: progress.total.load(Relaxed),
                }
            }
            Some(CliInstallState::Success(version)) => trouve_protocol::CliInstallStatus {
                status: "success".into(),
                version: Some(version.clone()),
                error: None,
                received_bytes: 0,
                total_bytes: 0,
            },
            Some(CliInstallState::Failed(e)) => trouve_protocol::CliInstallStatus {
                status: "failed".into(),
                version: None,
                error: Some(e.clone()),
                received_bytes: 0,
                total_bytes: 0,
            },
        }
    }

    // --- automations ------------------------------------------------------------

    /// All automations, in creation order.
    pub fn list_automations(&self) -> Result<Vec<trouve_protocol::Automation>, EngineError> {
        Ok(self.store.list_automations()?)
    }

    pub fn create_automation(
        &self,
        req: trouve_protocol::UpsertAutomationRequest,
    ) -> Result<trouve_protocol::Automation, EngineError> {
        self.validate_automation(&req)?;
        let next_run_at = if req.enabled {
            crate::automations::next_run(&req.schedule, chrono::Local::now())
                .map(|t| t.to_rfc3339())
        } else {
            None
        };
        let automation = trouve_protocol::Automation {
            id: new_id("auto"),
            name: req.name,
            prompt: req.prompt,
            workspace_id: req.workspace_id,
            mode: req.mode,
            model: req.model,
            thinking_level: req.thinking_level,
            permission_mode: req.permission_mode,
            schedule: req.schedule,
            enabled: req.enabled,
            next_run_at,
            last_run_at: None,
            last_session_id: None,
            last_error: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.store.insert_automation(&automation)?;
        Ok(automation)
    }

    pub fn update_automation(
        &self,
        id: &str,
        req: trouve_protocol::UpsertAutomationRequest,
    ) -> Result<trouve_protocol::Automation, EngineError> {
        self.validate_automation(&req)?;
        let mut automation = self
            .store
            .automation(id)?
            .ok_or_else(|| EngineError::NotFound(format!("automation {id}")))?;
        automation.name = req.name;
        automation.prompt = req.prompt;
        automation.workspace_id = req.workspace_id;
        automation.mode = req.mode;
        automation.model = req.model;
        automation.thinking_level = req.thinking_level;
        automation.permission_mode = req.permission_mode;
        automation.schedule = req.schedule;
        automation.enabled = req.enabled;
        automation.next_run_at = if req.enabled {
            crate::automations::next_run(&automation.schedule, chrono::Local::now())
                .map(|t| t.to_rfc3339())
        } else {
            None
        };
        self.store.update_automation(&automation)?;
        Ok(automation)
    }

    pub fn delete_automation(&self, id: &str) -> Result<(), EngineError> {
        if !self.store.delete_automation(id)? {
            return Err(EngineError::NotFound(format!("automation {id}")));
        }
        Ok(())
    }

    fn validate_automation(
        &self,
        req: &trouve_protocol::UpsertAutomationRequest,
    ) -> Result<(), EngineError> {
        if req.name.trim().is_empty() {
            return Err(EngineError::BadRequest("automations need a name".into()));
        }
        if req.prompt.trim().is_empty() {
            return Err(EngineError::BadRequest("automations need a prompt".into()));
        }
        if req
            .thinking_level
            .as_deref()
            .is_some_and(|level| level.trim().is_empty())
        {
            return Err(EngineError::BadRequest(
                "automation thinking_level must not be empty".into(),
            ));
        }
        if self.store.open_workspace(&req.workspace_id)?.is_none() {
            return Err(EngineError::NotFound(format!(
                "workspace {}",
                req.workspace_id
            )));
        }
        if let Some(complaint) = crate::automations::validate(&req.schedule) {
            return Err(EngineError::BadRequest(complaint));
        }
        Ok(())
    }

    /// Fire an automation immediately, in the background (creating the
    /// worktree takes a moment). The outcome lands in `last_*` and an
    /// `automation.fired` event, same as a scheduled run.
    pub fn run_automation_now(self: &Arc<Self>, id: &str) -> Result<(), EngineError> {
        let automation = self
            .store
            .automation(id)?
            .ok_or_else(|| EngineError::NotFound(format!("automation {id}")))?;
        let engine = self.clone();
        tokio::spawn(async move {
            engine.fire_and_record(&automation).await;
        });
        Ok(())
    }

    /// Start the background scheduler (called once when serving). Runs
    /// missed while the server was down are skipped — every enabled
    /// automation's next fire is recomputed from "now" at startup.
    pub fn start_automation_scheduler(self: &Arc<Self>) {
        let engine = self.clone();
        tokio::spawn(async move {
            let now = chrono::Local::now();
            if let Ok(automations) = engine.store.list_automations() {
                for a in automations {
                    let next = a
                        .enabled
                        .then(|| crate::automations::next_run(&a.schedule, now))
                        .flatten()
                        .map(|t| t.to_rfc3339());
                    let _ = engine.store.set_automation_next_run(&a.id, next.as_deref());
                }
            }
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                engine.fire_due_automations().await;
            }
        });
    }

    async fn fire_due_automations(self: &Arc<Self>) {
        let Ok(automations) = self.store.list_automations() else {
            return;
        };
        let now = chrono::Utc::now();
        for automation in automations {
            if !automation.enabled {
                continue;
            }
            // Closing a workspace pauses its scheduled activity without
            // deleting or disabling the persisted automation. Reopening the
            // workspace makes it eligible on a later scheduler tick.
            if self
                .store
                .open_workspace(&automation.workspace_id)
                .ok()
                .flatten()
                .is_none()
            {
                continue;
            }
            let due = automation
                .next_run_at
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|next| next <= now);
            if due {
                self.fire_and_record(&automation).await;
            }
        }
    }

    /// One run: session + thread + prompt, then bookkeeping and the
    /// server-scope event clients refresh on.
    async fn fire_and_record(self: &Arc<Self>, automation: &trouve_protocol::Automation) {
        let next = crate::automations::next_run(&automation.schedule, chrono::Local::now())
            .filter(|_| automation.enabled)
            .map(|t| t.to_rfc3339());
        let ran_at = chrono::Utc::now().to_rfc3339();
        match self.fire_automation(automation).await {
            Ok((session_id, thread_id, turn)) => {
                // Advance the schedule as soon as dispatch succeeds so a
                // long-running or approval-blocked turn cannot fire again on
                // the next scheduler tick. The completion watcher records
                // the actual outcome and only then emits automation.fired.
                let _ = self.store.mark_automation_run(
                    &automation.id,
                    &ran_at,
                    Some(&session_id),
                    "",
                    next.as_deref(),
                );
                let engine = self.clone();
                let automation_id = automation.id.clone();
                let automation_name = automation.name.clone();
                tokio::spawn(async move {
                    engine
                        .monitor_automation_turn(
                            automation_id,
                            automation_name,
                            session_id.clone(),
                            thread_id,
                            turn,
                        )
                        .await;
                });
            }
            Err(e) => {
                let error = e.to_string();
                let _ = self.store.mark_automation_run(
                    &automation.id,
                    &ran_at,
                    None,
                    &error,
                    next.as_deref(),
                );
                tracing::warn!("automation {} failed: {error}", automation.name);
                let _ = self.store.append_event(
                    Scope::Server,
                    Event::AutomationFired {
                        automation_id: automation.id.clone(),
                        session_id: None,
                        error,
                    },
                );
            }
        }
    }

    async fn fire_automation(
        self: &Arc<Self>,
        automation: &trouve_protocol::Automation,
    ) -> Result<(String, String, u64), EngineError> {
        let session = self
            .create_session(trouve_protocol::CreateSessionRequest {
                workspace_id: automation.workspace_id.clone(),
                idempotency_key: None,
                title: Some(format!(
                    "{} — {}",
                    automation.name,
                    chrono::Local::now().format("%b %d %H:%M")
                )),
                base_ref: None,
                checkout_ref: None,
                fetch_latest: true,
            })
            .await?;
        let mut model_options = serde_json::Map::new();
        if let Some(thinking_level) = automation.thinking_level.as_ref() {
            model_options.insert(
                "thinking_level".into(),
                serde_json::Value::String(thinking_level.clone()),
            );
        }
        let thread = self.create_thread(trouve_protocol::CreateThreadRequest {
            session_id: session.id.clone(),
            title: Some(automation.name.clone()),
            mode: automation.mode.clone(),
            model: automation.model.clone(),
            model_options,
            // Scoped to this fresh run session; it does not change global
            // mode defaults or carry approvals into future runs.
            permission_mode: Some(automation.permission_mode),
        })?;
        let accepted = self.send_message(&thread.id, automation.prompt.clone(), Vec::new())?;
        if accepted.queued || accepted.turn == 0 {
            return Err(EngineError::Conflict(format!(
                "automation thread {} did not dispatch",
                thread.id
            )));
        }
        Ok((session.id, thread.id, accepted.turn))
    }

    async fn monitor_automation_turn(
        self: &Arc<Self>,
        automation_id: String,
        automation_name: String,
        session_id: String,
        thread_id: String,
        turn: u64,
    ) {
        let scope = Scope::Thread(thread_id);
        let mut live = self.store.subscribe_scope(&scope);
        let mut after = 0u64;
        let mut replay = std::collections::VecDeque::from(
            self.store.events_after(&scope, after).unwrap_or_default(),
        );
        let error = loop {
            let envelope = match replay.pop_front() {
                Some(envelope) => envelope,
                None => match live.recv().await {
                    Ok(envelope) => envelope,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        replay = std::collections::VecDeque::from(
                            self.store.events_after(&scope, after).unwrap_or_default(),
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break "event stream closed before automation completed".to_string();
                    }
                },
            };
            if envelope.scope != scope || envelope.cursor <= after {
                continue;
            }
            after = envelope.cursor;
            match envelope.event {
                Event::ApprovalRequested {
                    turn: event_turn, ..
                } if event_turn == turn => {
                    // Ask remains the safe default for scheduled work; make
                    // its blocked state explicit instead of reporting a
                    // successful automation before the user responds.
                    let _ = self
                        .store
                        .set_automation_result(&automation_id, "awaiting approval");
                }
                Event::ApprovalResolved { .. } => {
                    let _ = self.store.set_automation_result(&automation_id, "");
                }
                Event::TurnCompleted {
                    turn: event_turn, ..
                } if event_turn == turn => break String::new(),
                Event::TurnFailed {
                    turn: event_turn,
                    error,
                } if event_turn == turn => break error,
                Event::TurnCancelled { turn: event_turn } if event_turn == turn => {
                    break "turn cancelled".to_string();
                }
                _ => {}
            }
        };

        let _ = self.store.set_automation_result(&automation_id, &error);
        if !error.is_empty() {
            tracing::warn!("automation {automation_name} failed: {error}");
        }
        let _ = self.store.append_event(
            Scope::Server,
            Event::AutomationFired {
                automation_id,
                session_id: Some(session_id),
                error,
            },
        );
    }

    // --- local models ---------------------------------------------------------

    /// The hardware probe result, run once (off the async runtime) and
    /// cached for the engine's lifetime.
    async fn hardware(&self) -> crate::local::Hardware {
        if self.hardware.get().is_none() {
            let hw = tokio::task::spawn_blocking(crate::local::probe_hardware)
                .await
                .unwrap_or_default();
            let _ = self.hardware.set(hw);
        }
        self.hardware.get().cloned().unwrap_or_default()
    }

    /// Local inference status for the settings screen: hardware, runtime
    /// install state, the running sidecar, and every model with its
    /// download/fit state.
    pub async fn local_status(&self) -> trouve_protocol::LocalStatus {
        use trouve_agents::install as cli;
        let hw = self.hardware().await;
        let managed = cli::installed(&self.data_dir, cli::CliId::LlamaServer);
        let (runtime_installed, runtime_version, runtime_managed) = match &managed {
            Some(info) => (true, Some(info.version.clone()), true),
            None => (false, None, false),
        };
        let runtime_latest_version = self.cli_latest_version(cli::CliId::LlamaServer).await;
        let runtime_update_available = match (&managed, &runtime_latest_version) {
            (Some(info), Some(latest)) => !cli_version_matches(&info.version, latest),
            _ => false,
        };
        let (running_model, server_status) = match self.local_manager.state() {
            crate::local::ServerState::Stopped => (None, "stopped".to_string()),
            crate::local::ServerState::Starting(m) => (Some(m), "starting".to_string()),
            crate::local::ServerState::Running(m) => (Some(m), "running".to_string()),
        };
        let enabled = self.config.lock().unwrap().local_enabled.unwrap_or(true);
        let downloads = self.local_downloads.lock().unwrap().clone();
        let models = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .map(|entry| {
                let path = crate::local::gguf_path(&self.data_dir, &entry);
                let downloaded = path.exists();
                let metadata = if downloaded {
                    crate::local::model_metadata(&path)
                } else {
                    Default::default()
                };
                let (download_status, download_bytes, download_error) =
                    match downloads.get(&entry.id) {
                        Some(LocalDownloadState::Pending { bytes, .. }) => (
                            "pending".to_string(),
                            bytes.load(std::sync::atomic::Ordering::Relaxed),
                            String::new(),
                        ),
                        Some(LocalDownloadState::Failed(e)) => ("failed".to_string(), 0, e.clone()),
                        None => ("none".to_string(), 0, String::new()),
                    };
                trouve_protocol::LocalModelInfo {
                    id: entry.id.clone(),
                    display_name: entry.display_name.clone(),
                    repo: entry.repo.clone(),
                    file: entry.file.clone(),
                    size_bytes: entry.size_bytes,
                    params: entry.params.clone(),
                    context_window: self
                        .local_manager
                        .context_window(&entry.id)
                        .unwrap_or(metadata.context_window),
                    fit: crate::local::fit(entry.size_bytes, &hw).to_string(),
                    notes: entry.notes.clone(),
                    downloaded,
                    download_status,
                    download_bytes,
                    download_error,
                    custom: entry.custom,
                }
            })
            .collect();
        trouve_protocol::LocalStatus {
            enabled,
            ram_bytes: hw.ram_bytes,
            gpus: hw.gpus,
            runtime_installed,
            runtime_version,
            runtime_managed,
            runtime_latest_version,
            runtime_update_available,
            running_model,
            server_status,
            models,
        }
    }

    /// Turn the built-in "local" provider on or off. Disabling stops the
    /// llama-server sidecar and removes the provider (its models disappear
    /// from pickers); enabling re-registers it. Persisted in config.toml.
    pub async fn set_local_enabled(&self, enabled: bool) -> Result<(), EngineError> {
        {
            let mut config = self.config.lock().unwrap();
            if config.local_enabled.unwrap_or(true) == enabled {
                return Ok(());
            }
            config.local_enabled = Some(enabled);
            self.persist_config(&config);
        }
        if enabled {
            self.injected_providers
                .lock()
                .unwrap()
                .insert("local".into(), self.local_provider.clone());
            self.providers
                .write()
                .unwrap()
                .insert("local".into(), self.local_provider.clone());
        } else {
            self.injected_providers.lock().unwrap().remove("local");
            self.providers.write().unwrap().remove("local");
            self.local_manager.stop().await;
            self.title_model.local_model_stopped().await;
        }
        Ok(())
    }

    /// Start downloading one model's GGUF from HuggingFace into the data
    /// dir. Progress is visible through `local_status`.
    pub fn start_local_model_download(self: &Arc<Self>, id: &str) -> Result<(), EngineError> {
        let entry = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| EngineError::NotFound(format!("local model {id}")))?;
        let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut downloads = self.local_downloads.lock().unwrap();
            if matches!(downloads.get(id), Some(LocalDownloadState::Pending { .. })) {
                return Err(EngineError::Conflict(format!(
                    "a download for {id} is already in progress"
                )));
            }
            downloads.insert(
                id.to_string(),
                LocalDownloadState::Pending {
                    bytes: counter.clone(),
                    cancel: cancel.clone(),
                },
            );
        }
        let engine = self.clone();
        let id_owned = id.to_string();
        tokio::spawn(async move {
            let result = download_gguf(&engine.data_dir, &entry, &counter, &cancel).await;
            let mut downloads = engine.local_downloads.lock().unwrap();
            match result {
                // Downloaded state comes from the file's existence;
                // cancelled downloads also just clear (status "none").
                Ok(_) => {
                    downloads.remove(&id_owned);
                }
                Err(e) => {
                    downloads.insert(id_owned, LocalDownloadState::Failed(format!("{e:#}")));
                }
            }
        });
        Ok(())
    }

    /// Ask an in-flight model download to stop; its partial file is
    /// deleted and the model returns to the not-downloaded state.
    pub fn cancel_local_model_download(&self, id: &str) -> Result<(), EngineError> {
        match self.local_downloads.lock().unwrap().get(id) {
            Some(LocalDownloadState::Pending { cancel, .. }) => {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            _ => Err(EngineError::NotFound(format!(
                "no download for {id} is in progress"
            ))),
        }
    }

    /// Register a custom GGUF (HuggingFace repo + filename), validating
    /// that the file exists and reading its size.
    pub async fn add_local_model(
        &self,
        req: trouve_protocol::AddLocalModelRequest,
    ) -> Result<(), EngineError> {
        let config_dir = self
            .config_dir
            .clone()
            .ok_or_else(|| EngineError::Internal(anyhow::anyhow!("no config dir")))?;
        let repo = req.repo.trim().trim_matches('/').to_string();
        let file = req.file.trim().trim_start_matches('/').to_string();
        if repo.is_empty() || !repo.contains('/') || file.is_empty() {
            return Err(EngineError::BadRequest(
                "expected a HuggingFace repo like owner/name and a .gguf filename".into(),
            ));
        }
        if !file.ends_with(".gguf") {
            return Err(EngineError::BadRequest("the file must be a .gguf".into()));
        }
        let id = crate::local::slug_from_file(&file);
        if crate::local::all_entries(Some(&config_dir))
            .iter()
            .any(|e| e.id == id)
        {
            return Err(EngineError::Conflict(format!(
                "a local model with id {id} already exists"
            )));
        }
        // Validate against HF and learn the size for the fit label. Don't
        // follow the CDN redirect: the size lives in `x-linked-size` on the
        // resolve response itself, and a redirect already proves existence.
        let url = crate::local::download_url(&repo, &file);
        let resp = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| EngineError::Internal(e.into()))?
            .head(&url)
            .send()
            .await
            .map_err(|e| EngineError::BadRequest(format!("checking {repo}/{file}: {e}")))?;
        if !resp.status().is_success() && !resp.status().is_redirection() {
            return Err(EngineError::BadRequest(format!(
                "HuggingFace returned {} for {repo}/{file} — check the repo and filename \
                 (gated repos are not supported)",
                resp.status()
            )));
        }
        let size_bytes = resp
            .headers()
            .get("x-linked-size")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .or_else(|| resp.content_length().filter(|n| *n > 0))
            .unwrap_or(0);
        let display_name = req
            .display_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        let path = crate::local::custom_models_path(&config_dir);
        let mut models = crate::local::read_custom_models(&path);
        models.push(crate::local::CustomModel {
            id,
            display_name,
            repo,
            file,
            size_bytes,
        });
        crate::local::write_custom_models(&path, &models)
            .map_err(|e| EngineError::Internal(e.into()))?;
        Ok(())
    }

    /// Search HuggingFace for GGUF repos matching `query`, listing each
    /// repo's single-file GGUFs with hardware-fit guidance and a
    /// recommended pick for this machine. Repos without usable files (or
    /// whose file listing fails) are dropped.
    pub async fn search_local_models(
        &self,
        query: &str,
    ) -> Result<Vec<trouve_protocol::LocalSearchResult>, EngineError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let hw = self.hardware().await;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(12))
            .build()
            .map_err(|e| EngineError::Internal(e.into()))?;
        let repos = crate::local::search_hf_repos(&client, query, 8)
            .await
            .map_err(|e| EngineError::BadRequest(format!("HuggingFace search failed: {e}")))?;
        // (repo, file) pairs already in the model list, to mark "added".
        let existing: std::collections::HashSet<(String, String)> =
            crate::local::all_entries(self.config_dir.as_deref())
                .into_iter()
                .map(|e| (e.repo.to_ascii_lowercase(), e.file.to_ascii_lowercase()))
                .collect();

        let lookups = repos.into_iter().map(|repo| {
            let client = client.clone();
            async move {
                let files = crate::local::list_gguf_files(&client, &repo.id)
                    .await
                    .ok()?;
                Some((repo, files))
            }
        });
        let mut results = Vec::new();
        for looked_up in futures::future::join_all(lookups).await {
            let Some((repo, mut files)) = looked_up else {
                continue;
            };
            if files.is_empty() {
                continue;
            }
            files.sort_by_key(|(_, size)| *size);
            let files: Vec<trouve_protocol::LocalSearchFile> = files
                .into_iter()
                .map(|(file, size_bytes)| trouve_protocol::LocalSearchFile {
                    quant: crate::local::quant_of(&file),
                    fit: crate::local::fit(size_bytes, &hw).to_string(),
                    added: existing
                        .contains(&(repo.id.to_ascii_lowercase(), file.to_ascii_lowercase())),
                    file,
                    size_bytes,
                })
                .collect();
            let recommended = recommend_gguf(&files) as u32;
            results.push(trouve_protocol::LocalSearchResult {
                repo: repo.id,
                downloads: repo.downloads,
                likes: repo.likes,
                files,
                recommended,
            });
        }
        Ok(results)
    }

    /// Delete a model's downloaded GGUF (stopping the server if it is the
    /// one loaded); custom entries are removed from the list entirely.
    pub async fn delete_local_model(&self, id: &str) -> Result<(), EngineError> {
        let entry = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| EngineError::NotFound(format!("local model {id}")))?;
        if self.local_manager.running_model().as_deref() == Some(id) {
            self.local_manager.stop().await;
            self.title_model.local_model_stopped().await;
        }
        self.local_downloads.lock().unwrap().remove(id);
        let gguf = crate::local::gguf_path(&self.data_dir, &entry);
        let _ = std::fs::remove_file(gguf.with_extension("gguf.part"));
        if gguf.exists() {
            std::fs::remove_file(&gguf).map_err(|e| EngineError::Internal(e.into()))?;
        }
        if entry.custom
            && let Some(config_dir) = &self.config_dir
        {
            let path = crate::local::custom_models_path(config_dir);
            let mut models = crate::local::read_custom_models(&path);
            models.retain(|m| m.id != id);
            crate::local::write_custom_models(&path, &models)
                .map_err(|e| EngineError::Internal(e.into()))?;
        }
        Ok(())
    }

    /// Stop the llama-server sidecar (frees the model's RAM/VRAM; the next
    /// local turn restarts it).
    pub async fn stop_local_server(&self) {
        self.local_manager.stop().await;
        self.title_model.local_model_stopped().await;
    }

    /// Restart the llama-server sidecar with the model it is serving. The
    /// reload happens in the background (large GGUFs take a while);
    /// progress shows in `local_status` as server_status "starting".
    pub async fn restart_local_server(&self) -> Result<(), EngineError> {
        let model = self
            .local_manager
            .running_model()
            .ok_or_else(|| EngineError::Conflict("no local server is running".into()))?;
        let entry = crate::local::all_entries(self.config_dir.as_deref())
            .into_iter()
            .find(|e| e.id == model)
            .ok_or_else(|| EngineError::NotFound(format!("local model {model}")))?;
        let bin = crate::local::runtime_bin(&self.data_dir).ok_or_else(|| {
            EngineError::Conflict("the llama.cpp runtime is not installed".into())
        })?;
        let gguf = crate::local::gguf_path(&self.data_dir, &entry);
        let log_path = self.data_dir.join("llama-server.log");
        self.local_manager.stop().await;
        let manager = self.local_manager.clone();
        tokio::spawn(async move {
            if let Err(e) = manager.ensure(&bin, &entry.id, &gguf, &log_path).await {
                tracing::warn!("llama-server restart failed: {e:#}");
            }
        });
        Ok(())
    }

    /// Report the state of an OAuth login started with `start_login`.
    pub fn login_status(&self, id: &str) -> trouve_protocol::LoginStatus {
        match self.logins.lock().unwrap().get(id) {
            None => trouve_protocol::LoginStatus {
                status: "none".into(),
                error: None,
            },
            Some(LoginState::Pending { .. }) => trouve_protocol::LoginStatus {
                status: "pending".into(),
                error: None,
            },
            Some(LoginState::Success) => trouve_protocol::LoginStatus {
                status: "success".into(),
                error: None,
            },
            Some(LoginState::Failed(e)) => trouve_protocol::LoginStatus {
                status: "failed".into(),
                error: Some(e.clone()),
            },
        }
    }

    /// Forward a browser authentication response to an interactive vendor CLI.
    pub async fn complete_login(
        &self,
        id: &str,
        request: trouve_protocol::CompleteLoginRequest,
    ) -> Result<trouve_protocol::LoginStatus, EngineError> {
        let callback = request.callback_url.trim();
        if callback.is_empty() {
            return Err(EngineError::BadRequest(
                "authentication response must not be empty".into(),
            ));
        }
        if callback.len() > 16 * 1024 || callback.chars().any(char::is_control) {
            return Err(EngineError::BadRequest(
                "authentication response is too long or contains control characters".into(),
            ));
        }

        let sender = {
            let mut logins = self.logins.lock().unwrap();
            match logins.get_mut(id) {
                Some(LoginState::Pending {
                    callback_sender, ..
                }) => callback_sender.take().ok_or_else(|| {
                    EngineError::Conflict(format!(
                        "provider {id} login does not accept an authentication response"
                    ))
                })?,
                Some(LoginState::Success) => {
                    return Ok(trouve_protocol::LoginStatus {
                        status: "success".into(),
                        error: None,
                    });
                }
                Some(LoginState::Failed(error)) => {
                    return Err(EngineError::Conflict(format!(
                        "provider {id} login already failed: {error}"
                    )));
                }
                None => {
                    return Err(EngineError::NotFound(format!(
                        "no login is running for provider {id}"
                    )));
                }
            }
        };
        sender.send(callback.to_string()).await.map_err(|_| {
            EngineError::Conflict(format!("provider {id} login is no longer accepting input"))
        })?;
        Ok(self.login_status(id))
    }

    fn finish_login(
        &self,
        id: &str,
        result: Result<trouve_providers::auth::OAuthTokens, trouve_providers::ProviderError>,
    ) {
        let state = match result {
            Ok(tokens) => match serde_json::to_string(&tokens)
                .map_err(anyhow::Error::from)
                .and_then(|raw| {
                    self.secrets
                        .set(&trouve_providers::secrets::oauth_secret(id), &raw)
                }) {
                Ok(()) => {
                    self.reload_providers();
                    LoginState::Success
                }
                Err(e) => LoginState::Failed(format!("storing tokens: {e}")),
            },
            Err(e) => LoginState::Failed(e.to_string()),
        };
        let authenticated = matches!(state, LoginState::Success);
        self.logins.lock().unwrap().insert(id.to_string(), state);
        let github_host = if id == "github" {
            Some(crate::github::GITHUB_COM)
        } else {
            id.strip_prefix("github:")
        };
        if authenticated && let Some(host) = github_host {
            match self.store.wake_authenticated_session_pr_verifications(host) {
                Ok(woken) if woken > 0 => self.session_pr_verification_wake.notify_one(),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    %host,
                    %error,
                    "failed to wake pull request verification after GitHub login"
                ),
            }
        }
    }

    /// Set the default model for new threads (provider-qualified).
    pub fn set_default_model(
        &self,
        model: &str,
        thinking_level: Option<&str>,
    ) -> Result<(), EngineError> {
        if !model.contains('/') {
            return Err(EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            )));
        }
        validate_thinking_level(thinking_level)?;
        {
            let mut config = self.config.lock().unwrap();
            config.default_model = Some(model.to_string());
            if let Some(level) = thinking_level {
                config.default_thinking_level = Some(level.into());
            }
            self.persist_config(&config);
            let mut defaults = self.global_defaults.write().unwrap();
            defaults.model = model.to_string();
            if let Some(level) = thinking_level {
                defaults.thinking_level = Some(level.into());
            }
        }
        Ok(())
    }

    /// Validate, persist, and apply all global defaults as one replacement.
    pub fn set_global_defaults(
        &self,
        model: &str,
        thinking_level: Option<&str>,
        permission_mode: trouve_protocol::PermissionMode,
    ) -> Result<(), EngineError> {
        if !model.contains('/') {
            return Err(EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            )));
        }
        validate_thinking_level(thinking_level)?;

        let next_defaults = GlobalDefaults {
            model: model.to_string(),
            thinking_level: thinking_level.map(String::from),
            permission_mode,
        };
        let mut config = self.config.lock().unwrap();
        let mut next_config = config.clone();
        next_config.default_model = Some(next_defaults.model.clone());
        next_config.default_thinking_level = next_defaults.thinking_level.clone();
        next_config.default_permission_mode = Some(next_defaults.permission_mode);
        if let Some(path) = &self.config_file {
            next_config
                .save_to(path)
                .with_context(|| format!("persisting global defaults to {}", path.display()))?;
        }
        let mut defaults = self.global_defaults.write().unwrap();
        *config = next_config;
        *defaults = next_defaults;
        Ok(())
    }

    /// Set the global default permission mode for new threads (used by
    /// modes that don't set one of their own).
    pub fn set_default_permission_mode(
        &self,
        mode: trouve_protocol::PermissionMode,
    ) -> Result<(), EngineError> {
        {
            let mut config = self.config.lock().unwrap();
            config.default_permission_mode = Some(mode);
            self.persist_config(&config);
            self.global_defaults.write().unwrap().permission_mode = mode;
        }
        Ok(())
    }

    /// Current settings and runtime state for session naming.
    pub fn git_worktree_settings(&self) -> trouve_protocol::GitWorktreeSettings {
        self.title_model.settings()
    }

    /// Current settings paired with the server cursor they are at least as
    /// fresh as. Read the cursor first so a concurrent status change can only
    /// make the returned settings newer than the cursor, never older.
    pub fn git_worktree_settings_snapshot(
        &self,
    ) -> Result<(u64, trouve_protocol::GitWorktreeSettings), EngineError> {
        let cursor = self
            .store
            .latest_event_cursor(&trouve_protocol::Scope::Server)?;
        Ok((cursor, self.git_worktree_settings()))
    }

    /// Persist and immediately apply session-title lifecycle and placement.
    pub async fn set_git_worktree_settings(
        &self,
        behavior: trouve_protocol::TitleModelLoadBehavior,
        resources: trouve_protocol::TitleModelResourcePolicy,
        derive_branch_name_from_session_title: Option<bool>,
    ) -> Result<trouve_protocol::GitWorktreeSettings, EngineError> {
        if resources == trouve_protocol::TitleModelResourcePolicy::GpuOnly
            && self.hardware().await.gpus.is_empty()
        {
            return Err(EngineError::BadRequest(
                "GPU-only session naming requires a detected GPU".into(),
            ));
        }
        let _transition = self.title_model_behavior_transition.lock().await;
        let derive_branch_name_from_session_title = derive_branch_name_from_session_title
            .unwrap_or_else(|| self.title_model.derive_branch_name_from_session_title());
        {
            let mut config = self.config.lock().unwrap();
            config.title_model_load_behavior = Some(behavior);
            config.title_model_resource_policy = Some(resources);
            config.derive_branch_name_from_session_title =
                Some(derive_branch_name_from_session_title);
            self.persist_config(&config);
        }
        self.title_model
            .set_configuration(behavior, resources, derive_branch_name_from_session_title)
            .await;
        Ok(self.git_worktree_settings())
    }

    /// Warm the dedicated title model according to its configured lifecycle.
    /// This is non-blocking and is safe to call once the Tokio runtime exists.
    pub fn warm_title_model(&self) {
        self.title_model.warm_on_start();
    }

    pub fn install_title_model(self: &Arc<Self>) -> Result<(), EngineError> {
        let engine = Arc::downgrade(self);
        self.title_model
            .start_install(move || {
                if let Some(engine) = engine.upgrade() {
                    engine.reload_providers();
                    engine
                        .cli_latest
                        .lock()
                        .unwrap()
                        .remove(trouve_agents::install::CliId::LlamaServer.as_str());
                }
            })
            .map_err(|error| EngineError::Conflict(error.to_string()))?;
        Ok(())
    }

    pub fn cancel_title_model_install(&self) -> Result<(), EngineError> {
        match self.title_model.cancel_install() {
            Ok(()) => Ok(()),
            Err(error) if error.is::<crate::title_model::NoInstallInProgress>() => {
                Err(EngineError::NotFound(error.to_string()))
            }
            Err(error) => Err(EngineError::Conflict(error.to_string())),
        }
    }

    /// Derive a title without ever blocking session creation on optional
    /// model assets or model-quality failures.
    pub async fn generate_session_title(
        &self,
        prompt: &str,
    ) -> trouve_protocol::GeneratedSessionTitle {
        let title_model = self.title_model.clone();
        let prompt_owned = prompt.to_string();
        let generated = match tokio::time::timeout(SESSION_TITLE_TIMEOUT, async {
            let mut generation = self.title_model_generation.lock().await;
            if let Some(previous) = generation.as_mut() {
                previous.abort();
                let _ = previous.await;
            }
            generation.take();

            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            *generation = Some(tokio::spawn(async move {
                let _ = result_tx.send(title_model.generate(&prompt_owned).await);
            }));
            drop(generation);

            result_rx
                .await
                .map_err(|error| anyhow!("session title task failed: {error}"))?
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow!("session title generation timed out")),
        };
        match generated {
            Ok(title) => trouve_protocol::GeneratedSessionTitle {
                title,
                source: "model".into(),
            },
            Err(error) => {
                tracing::debug!("using heuristic session title: {error:#}");
                trouve_protocol::GeneratedSessionTitle {
                    title: crate::title::summarize_session_title(prompt),
                    source: "heuristic".into(),
                }
            }
        }
    }

    async fn generate_subagent_title(
        &self,
        supplied_name: Option<&str>,
        prompt: Option<&str>,
    ) -> Option<String> {
        let name = match supplied_name.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => name.to_string(),
            None => {
                let prompt = prompt.map(str::trim).filter(|prompt| !prompt.is_empty())?;
                self.generate_session_title(prompt).await.title
            }
        };
        let normalized = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            None
        } else if normalized.starts_with("Subagent:") {
            Some(normalized)
        } else {
            Some(format!("Subagent: {normalized}"))
        }
    }

    pub(crate) fn persist_config(&self, config: &Config) {
        if let Some(path) = &self.config_file
            && let Err(e) = config.save_to(path)
        {
            tracing::warn!("failed to persist config: {e}");
        }
    }

    /// Rebuild the provider registry from the current config (after provider
    /// CRUD), preserving programmatically injected providers.
    fn reload_providers(&self) {
        let config = self.config.lock().unwrap().clone();
        let mut rebuilt = build_all_providers(&config, &self.secrets, &self.model_catalog);
        for (id, p) in self.injected_providers.lock().unwrap().iter() {
            rebuilt.insert(id.clone(), p.clone());
        }
        *self.providers.write().unwrap() = rebuilt;
        let mut backends =
            build_all_backends(&config, &self.secrets, &self.data_dir, &self.model_catalog);
        for (id, b) in self.injected_backends.lock().unwrap().iter() {
            backends.insert(id.clone(), b.clone());
        }
        *self.backends.write().unwrap() = backends;
        // Rebuilt backends carry fresh background-turn signal channels; hand
        // their receivers to the listener pump so autonomous-turn attachment
        // survives the reload (the old instances' forwarders end when their
        // senders drop).
        self.intake_background_turn_signals();
    }

    pub fn thread_usage(
        &self,
        thread_id: &str,
    ) -> Result<trouve_protocol::UsageSummary, EngineError> {
        self.get_thread(thread_id)?;
        Ok(self
            .store
            .usage_summary(crate::store::UsageScope::Thread(thread_id))?)
    }

    pub fn session_usage(
        &self,
        session_id: &str,
    ) -> Result<trouve_protocol::UsageSummary, EngineError> {
        self.get_session(session_id)?;
        Ok(self
            .store
            .usage_summary(crate::store::UsageScope::Session(session_id))?)
    }

    /// Agent personas visible for a workspace. This is the unified catalog:
    /// interactive personas plus the focused code-review personas.
    pub fn list_personas(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<AgentPersona>, EngineError> {
        let root = match workspace_id {
            Some(id) => {
                let ws = self
                    .store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
                Some(PathBuf::from(ws.path))
            }
            None => None,
        };
        self.resolve_personas(root.as_deref())
    }

    /// Personas with provenance (builtin / customized / custom / workspace)
    /// for the settings screen.
    pub fn list_persona_infos(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<trouve_protocol::PersonaInfo>, EngineError> {
        let root = match workspace_id {
            Some(id) => {
                let ws = self
                    .store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
                Some(PathBuf::from(ws.path))
            }
            None => None,
        };
        let mut infos =
            personas::resolve_persona_infos(self.config_dir.as_deref(), root.as_deref());
        let reviewer_catalog = self.code_review_reviewer_catalog()?;
        let mut builtin_order = HashMap::new();
        for id in personas::builtin_personas()
            .into_iter()
            .map(|persona| persona.id)
            .chain(
                crate::reviewers::built_in_reviewers()
                    .into_iter()
                    .map(|reviewer| reviewer.id),
            )
            .chain(
                reviewer_catalog
                    .iter()
                    .filter(|reviewer| reviewer.built_in)
                    .map(|reviewer| reviewer.id.clone()),
            )
        {
            let position = builtin_order.len();
            builtin_order.entry(id).or_insert(position);
        }
        let builtin_ids: HashSet<_> = builtin_order.keys().cloned().collect();
        for info in &mut infos {
            if builtin_ids.contains(&info.persona.id) && info.origin == "custom" {
                info.origin = "customized".into();
            }
        }
        for reviewer in reviewer_catalog {
            if infos.iter().any(|info| info.persona.id == reviewer.id) {
                continue;
            }
            infos.push(trouve_protocol::PersonaInfo {
                persona: crate::reviewers::reviewer_as_persona(&reviewer),
                origin: if reviewer.built_in {
                    "builtin"
                } else {
                    "custom"
                }
                .into(),
            });
        }
        infos.sort_by_key(|info| {
            (
                builtin_order
                    .get(&info.persona.id)
                    .copied()
                    .unwrap_or(usize::MAX),
                info.persona.id.clone(),
            )
        });
        Ok(infos)
    }

    fn resolve_personas(
        &self,
        workspace_root: Option<&Path>,
    ) -> Result<Vec<AgentPersona>, EngineError> {
        let resolved = personas::resolve_personas(self.config_dir.as_deref(), workspace_root);
        let mut personas: Vec<_> = self
            .code_review_reviewer_catalog_with_personas(resolved.clone())?
            .iter()
            .map(crate::reviewers::reviewer_as_persona)
            .collect();
        for persona in resolved {
            personas.retain(|candidate| candidate.id != persona.id);
            personas.push(persona);
        }
        Ok(personas)
    }

    /// Create or update a user-level persona. Saving under a built-in id
    /// customizes that built-in; the file lands in `<config>/personas/`.
    pub async fn upsert_persona(
        &self,
        id: &str,
        req: trouve_protocol::UpsertPersonaRequest,
    ) -> Result<(), EngineError> {
        let legacy_reviewer = !personas::is_valid_persona_id(id)
            && id.starts_with("custom:")
            && self
                .store
                .list_custom_reviewer_profiles()?
                .iter()
                .any(|reviewer| reviewer.id == id);
        if !legacy_reviewer {
            validate_persona_id(id)?;
        }
        let config_dir = self
            .config_dir
            .as_deref()
            .ok_or_else(|| EngineError::BadRequest("no config dir".into()))?;
        if let Some(model) = req.default_model.as_deref()
            && !model.contains('/')
        {
            return Err(EngineError::BadRequest(format!(
                "default_model must be provider-qualified (\"provider/model\"), got {model}"
            )));
        }
        validate_thinking_level(req.default_thinking_level.as_deref())?;
        let persona = AgentPersona {
            id: id.to_string(),
            display_name: req.display_name,
            group: req.group,
            system_prompt: req.system_prompt,
            allowed_tools: req.allowed_tools,
            read_only: req.read_only,
            default_permission_mode: req.default_permission_mode,
            default_model: req.default_model,
            default_thinking_level: req.default_thinking_level,
        };
        let mutation = self.persona_mutations.clone().lock_owned().await;
        if legacy_reviewer {
            if persona.group != trouve_protocol::PersonaGroup::Reviewer {
                return Err(EngineError::BadRequest(
                    "legacy reviewer personas cannot be moved to the general group".into(),
                ));
            }
            let existing = self
                .store
                .list_custom_reviewer_profiles()?
                .into_iter()
                .find(|reviewer| reviewer.id == id)
                .ok_or_else(|| {
                    EngineError::BadRequest(format!(
                        "legacy reviewer persona {id} no longer exists"
                    ))
                })?;
            let policy = crate::reviewers::reviewer_as_persona(&existing);
            if persona.allowed_tools != policy.allowed_tools
                || persona.read_only != policy.read_only
                || persona.default_permission_mode != policy.default_permission_mode
            {
                return Err(EngineError::BadRequest(
                    "legacy reviewer policy fields cannot be changed until the reviewer is migrated"
                        .into(),
                ));
            }
            let replacement_claim = if self.store.persona_deletion_pending(id)? {
                Some(self.store.claim_persona_deletion(id)?.ok_or_else(|| {
                    EngineError::BadRequest(format!("persona {id} is currently being deleted"))
                })?)
            } else {
                None
            };
            let reviewer = crate::reviewers::persona_as_reviewer(&persona, false);
            if let Some(claim) = replacement_claim {
                if let Err(error) = self
                    .store
                    .replace_claimed_reviewer_profile(&reviewer, &claim)
                {
                    let _ = self.store.release_persona_deletion_claim(id, &claim);
                    return Err(EngineError::Internal(error));
                }
            } else {
                self.store.upsert_reviewer_profile(&reviewer)?;
            }
            drop(mutation);
            return Ok(());
        }
        let current = self.resolve_personas(None)?;
        if personas::find_persona(&current, id).is_some_and(|existing| {
            existing.group == trouve_protocol::PersonaGroup::Reviewer
                && persona.group == trouve_protocol::PersonaGroup::General
        }) {
            let referenced = self
                .store
                .list_code_review_repositories()?
                .iter()
                .any(|repository| {
                    repository
                        .reviewer_ids
                        .iter()
                        .any(|candidate| candidate == id)
                        || repository
                            .included_reviewer_ids
                            .iter()
                            .any(|candidate| candidate == id)
                        || repository
                            .excluded_reviewer_ids
                            .iter()
                            .any(|candidate| candidate == id)
                        || repository
                            .reviewer_overrides
                            .iter()
                            .any(|entry| entry.reviewer_id == id)
                });
            if referenced {
                return Err(EngineError::BadRequest(format!(
                    "reviewer persona {id} is selected by a code review repository and cannot be moved to the general group"
                )));
            }
        }
        let replacement_claim = if self.store.persona_deletion_pending(id)? {
            Some(self.store.claim_persona_deletion(id)?.ok_or_else(|| {
                EngineError::BadRequest(format!("persona {id} is currently being deleted"))
            })?)
        } else {
            None
        };
        if let Some(claim) = replacement_claim {
            let store = self.store.clone();
            let executor = self.executor.clone();
            let config_dir = config_dir.to_path_buf();
            let id = id.to_string();
            tokio::spawn(async move {
                let _mutation = mutation;
                let write = executor.replace_persona_file(
                    &config_dir,
                    &persona,
                    store.clone(),
                    claim.clone(),
                );
                tokio::pin!(write);
                let period = Duration::from_secs(60);
                let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let write_result = loop {
                    tokio::select! {
                        result = &mut write => break result,
                        _ = interval.tick() => {
                            if let Err(error) = store.renew_persona_deletion_claim(&id, &claim) {
                                tracing::warn!(persona_id = %id, %error, "failed to renew persona replacement claim");
                            }
                        }
                    }
                };
                if let Err(error) = write_result {
                    store.release_persona_deletion_claim(&id, &claim)?;
                    return Err(EngineError::Internal(anyhow::anyhow!(error)));
                }
                Ok(())
            })
            .await
            .map_err(|error| EngineError::Internal(anyhow::anyhow!("persona replacement task failed: {error}")))??;
        } else {
            let executor = self.executor.clone();
            let config_dir = config_dir.to_path_buf();
            tokio::spawn(async move {
                let _mutation = mutation;
                executor
                    .upsert_persona_file(&config_dir, &persona)
                    .await
                    .map_err(|error| EngineError::Internal(anyhow::anyhow!(error)))?;
                Ok::<(), EngineError>(())
            })
            .await
            .map_err(|error| {
                EngineError::Internal(anyhow::anyhow!("persona upsert task failed: {error}"))
            })??;
        }
        Ok(())
    }

    /// Remove a user-level persona file: deletes a custom persona, or resets a
    /// customized built-in to its defaults.
    pub async fn delete_persona(&self, id: &str) -> Result<(), EngineError> {
        if !personas::is_valid_persona_id(id)
            && !(id.starts_with("custom:")
                && self
                    .store
                    .list_custom_reviewer_profiles()?
                    .iter()
                    .any(|reviewer| reviewer.id == id))
        {
            validate_persona_id(id)?;
        }
        let config_dir = self
            .config_dir
            .as_deref()
            .ok_or_else(|| EngineError::BadRequest("no config dir".into()))?;
        let mutation = self.persona_mutations.clone().lock_owned().await;
        let deletion_pending = self.store.persona_deletion_pending(id)?;
        let reviewer_catalog = self.code_review_reviewer_catalog()?;
        let custom_reviewer = reviewer_catalog
            .iter()
            .any(|reviewer| reviewer.id == id && !reviewer.built_in);
        if custom_reviewer
            && !personas::is_valid_persona_id(id)
            && personas::legacy_user_persona_file(config_dir, id)?
        {
            return Err(EngineError::BadRequest(format!(
                "legacy reviewer {id} is shadowed by a persona file; remove that file before deleting the reviewer"
            )));
        }
        let system_persona = personas::builtin_personas()
            .iter()
            .any(|persona| persona.id == id)
            || reviewer_catalog
                .iter()
                .any(|reviewer| reviewer.id == id && reviewer.built_in);
        let is_custom = deletion_pending
            || custom_reviewer
            || (!system_persona
                && personas::resolve_persona_infos(Some(config_dir), None)
                    .iter()
                    .any(|info| info.persona.id == id && info.origin == "custom"));
        if is_custom {
            self.store.begin_persona_deletion(id)?;
            let claim = self.store.claim_persona_deletion(id)?.ok_or_else(|| {
                EngineError::BadRequest(format!("persona {id} is currently being deleted"))
            })?;
            let store = self.store.clone();
            let executor = self.executor.clone();
            let config_dir = config_dir.to_path_buf();
            let id = id.to_string();
            let task_claim = claim.clone();
            let task_id = id.clone();
            let result = tokio::spawn(async move {
                let _mutation = mutation;
                let file_result = if custom_reviewer && !personas::is_valid_persona_id(&task_id) {
                    Ok(())
                } else {
                    executor
                        .delete_persona_file(
                            &config_dir,
                            &task_id,
                            deletion_pending || custom_reviewer,
                        )
                        .await
                };
                if let Err(error) = file_result {
                    store.release_persona_deletion_claim(&task_id, &task_claim)?;
                    return Err(EngineError::Internal(anyhow::anyhow!(error)));
                }
                if let Err(error) =
                    store.complete_claimed_persona_deletion_token(&task_id, &task_claim)
                {
                    store.release_persona_deletion_claim(&task_id, &task_claim)?;
                    return Err(EngineError::Internal(error));
                }
                Ok(())
            })
            .await;
            return match result {
                Ok(result) => result,
                Err(error) => {
                    self.store.release_persona_deletion_claim(&id, &claim)?;
                    Err(EngineError::Internal(anyhow::anyhow!(
                        "persona deletion task failed: {error}"
                    )))
                }
            };
        }
        if !system_persona {
            return Err(EngineError::BadRequest(format!(
                "persona {id} is not a user-configurable persona"
            )));
        }
        let has_override = personas::user_persona_file(config_dir, id)
            .map_err(EngineError::Internal)?
            .is_some();
        if !has_override {
            return Err(EngineError::BadRequest(format!(
                "persona {id} is a built-in with no user override to remove"
            )));
        }
        self.executor
            .delete_persona_file(config_dir, id, false)
            .await
            .map_err(|error| EngineError::Internal(anyhow::anyhow!(error)))
    }

    /// GitHub repository named by the session's origin remote. Routes to
    /// github.com or a configured GitHub Enterprise host based on the URL.
    fn github_repository_for_session(
        &self,
        session: &trouve_protocol::Session,
    ) -> Result<(String, String, String), EngineError> {
        self.github_repository_for_checkout(&PathBuf::from(&session.worktree_path))
    }

    /// GitHub repository named by any checkout's origin remote.
    fn github_repository_for_checkout(
        &self,
        checkout: &Path,
    ) -> Result<(String, String, String), EngineError> {
        let url = git::remote_url(checkout, "origin")
            .ok_or_else(|| EngineError::BadRequest("workspace has no 'origin' remote".into()))?;
        let (host, owner, repo) = crate::github::parse_remote(&url).ok_or_else(|| {
            EngineError::BadRequest(format!("origin is not a GitHub-style remote: {url}"))
        })?;
        if !self.github_hosts().iter().any(|(h, _)| *h == host) {
            return Err(EngineError::BadRequest(format!(
                "origin remote is on {host}, which isn't github.com or a configured \
                 GitHub Enterprise host — add it in Settings → Integrations"
            )));
        }
        Ok((host, owner, repo))
    }

    /// Authenticated GitHub client for the session's origin repository.
    fn github_for_session(
        &self,
        session: &trouve_protocol::Session,
    ) -> Result<crate::github::GitHub, EngineError> {
        self.github_for_checkout(&PathBuf::from(&session.worktree_path))
    }

    /// Authenticated GitHub client for any checkout's origin repository.
    fn github_for_checkout(&self, checkout: &Path) -> Result<crate::github::GitHub, EngineError> {
        let (host, owner, repo) = self.github_repository_for_checkout(checkout)?;
        let token = self.github_token(&host).ok_or_else(|| {
            EngineError::BadRequest(format!(
                "no GitHub OAuth session for {host}; sign in under Settings → Integrations"
            ))
        })?;
        crate::github::GitHub::new(&token, &host, &owner, &repo).map_err(EngineError::Internal)
    }

    /// Every GitHub host the integration knows: github.com first (always),
    /// then the configured enterprise hosts, each with its optional OAuth
    /// app client id.
    fn github_hosts(&self) -> Vec<(String, Option<String>)> {
        let config = self.config.lock().unwrap();
        let mut hosts = vec![(
            crate::github::GITHUB_COM.to_string(),
            // github.com always has an OAuth path: the built-in shared app,
            // unless config overrides it with the user's own client id.
            config
                .github_client_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .or_else(|| Some(crate::github::DEFAULT_CLIENT_ID.to_string())),
        )];
        for e in &config.github_enterprise {
            hosts.push((
                e.host.clone(),
                e.client_id.clone().filter(|id| !id.trim().is_empty()),
            ));
        }
        hosts
    }

    /// Secret-store / login id for a GitHub host. github.com keeps the
    /// plain "github" id (pre-enterprise secrets stay valid); enterprise
    /// hosts get "github:<host>".
    fn github_secret_id(host: &str) -> String {
        if host == crate::github::GITHUB_COM {
            "github".to_string()
        } else {
            format!("github:{host}")
        }
    }

    /// The OAuth access token for a host. GitHub authentication deliberately
    /// has one integration point: the device-flow secret.
    fn github_token(&self, host: &str) -> Option<String> {
        let id = Self::github_secret_id(host);
        if let Ok(Some(raw)) = self
            .secrets
            .get(&trouve_providers::secrets::oauth_secret(&id))
        {
            // Device-flow tokens from classic OAuth apps don't expire; apps
            // configured with expiring tokens just need a fresh sign-in.
            if let Ok(tokens) = serde_json::from_str::<trouve_providers::auth::OAuthTokens>(&raw) {
                return Some(tokens.access_token);
            }
        }
        None
    }

    /// Append durable links for PR numbers not already recorded for a session.
    fn record_session_pr_numbers(
        &self,
        session_id: &str,
        repository: &(String, String, String),
        numbers: impl IntoIterator<Item = u64>,
        recorded: &mut HashSet<u64>,
    ) -> Result<(), EngineError> {
        let (host, owner, repo) = repository;
        for number in numbers {
            if recorded.insert(number) {
                self.store.append_event(
                    Scope::Session(session_id.to_string()),
                    Event::SessionPrOpened {
                        number,
                        url: crate::github::pr_url(host, owner, repo, number),
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Branch-based discovery outside the tool-completion path still requires
    /// the fetched head to exist locally before it can become durable evidence.
    async fn pr_has_locally_verified_head(
        &self,
        session_id: &str,
        pr: &trouve_protocol::PrInfo,
    ) -> bool {
        let Some(commit) = pr.head_sha.clone() else {
            return false;
        };
        let lifecycle = self.session_lock(session_id);
        let _lifecycle_guard = lifecycle.read().await;
        if self.deleting_sessions.lock().unwrap().contains(session_id) {
            return false;
        }
        let Ok(Some(session)) = self.store.session(session_id) else {
            return false;
        };
        if session.branch != pr.head {
            return false;
        }
        let branch = session.branch.clone();
        let worktree = PathBuf::from(&session.worktree_path);
        tokio::task::spawn_blocking(move || {
            crate::git::checked_out_branch_descends_from(&worktree, &branch, &commit, &commit)
        })
        .await
        .unwrap_or(false)
    }

    /// Snapshot a coherent branch/HEAD ownership pair for a creator
    /// attestation. Callers capture before execution when possible and retain
    /// the immutable pair even if another turn later advances the branch.
    async fn capture_session_pr_head(session: &Session) -> Option<(String, String)> {
        for attempt in 0..3 {
            let worktree = PathBuf::from(&session.worktree_path);
            if let Some(evidence) =
                tokio::task::spawn_blocking(move || crate::git::checked_out_branch_head(&worktree))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .filter(|(branch, _)| branch == &session.branch)
            {
                return Some(evidence);
            }
            if attempt < 2 {
                tokio::task::yield_now().await;
            }
        }
        None
    }

    fn session_pr_verification_intents(
        session: &Session,
        repository: (String, String, String),
        priority_numbers: impl IntoIterator<Item = u64>,
        fallback_numbers: impl IntoIterator<Item = u64>,
        evidence: Option<(String, String)>,
    ) -> Vec<SessionPrVerificationIntent> {
        let Some((branch, head_sha)) = evidence else {
            return Vec::new();
        };
        if branch != session.branch {
            return Vec::new();
        }
        let (host, owner, repository) = repository;
        let mut numbers = Vec::new();
        let mut seen = HashSet::new();
        for candidates in [
            priority_numbers.into_iter().collect::<Vec<_>>(),
            fallback_numbers.into_iter().collect::<Vec<_>>(),
        ] {
            let mut candidates = candidates
                .into_iter()
                .filter(|number| *number > 0)
                .collect::<Vec<_>>();
            candidates.sort_unstable_by(|left, right| right.cmp(left));
            candidates.dedup();
            for number in candidates {
                if seen.insert(number) {
                    numbers.push(number);
                    if numbers.len() == MAX_SESSION_PR_VERIFICATIONS_PER_CREATION_CALL {
                        break;
                    }
                }
            }
            if numbers.len() == MAX_SESSION_PR_VERIFICATIONS_PER_CREATION_CALL {
                break;
            }
        }
        let created_at = chrono::Utc::now().to_rfc3339();
        numbers
            .into_iter()
            .map(|number| SessionPrVerificationIntent {
                session_id: session.id.clone(),
                host: host.clone(),
                owner: owner.clone(),
                repository: repository.clone(),
                number,
                branch: branch.clone(),
                head_sha: head_sha.clone(),
                attempts: 0,
                last_failure_class: String::new(),
                consecutive_failures: 0,
                created_at: created_at.clone(),
            })
            .collect()
    }

    fn session_pr_verification_expired(intent: &SessionPrVerificationIntent) -> bool {
        let Ok(created_at) = chrono::DateTime::parse_from_rfc3339(&intent.created_at) else {
            return true;
        };
        chrono::Utc::now().signed_duration_since(created_at)
            >= chrono::Duration::days(SESSION_PR_VERIFICATION_RETENTION_DAYS)
    }

    fn session_pr_verification_retry_exhausted(
        intent: &SessionPrVerificationIntent,
        failure_class: &str,
        max_consecutive_failures: Option<u32>,
        count_request: bool,
    ) -> bool {
        let next_requests = intent
            .attempts
            .saturating_add(if count_request { 1 } else { 0 });
        let next_consecutive_failures = if intent.last_failure_class == failure_class {
            intent.consecutive_failures.saturating_add(1)
        } else {
            1
        };
        next_requests >= MAX_SESSION_PR_REQUEST_ATTEMPTS
            || max_consecutive_failures.is_some_and(|maximum| next_consecutive_failures >= maximum)
    }

    fn session_pr_verification_retry_delay(
        intent: &SessionPrVerificationIntent,
        failure_class: &str,
        count_request: bool,
    ) -> i64 {
        if count_request {
            // Preserve monotonic request pacing even when GitHub alternates
            // between not-found, moved-head, and transport outcomes.
            return Store::session_pr_verification_retry_delay(intent.attempts);
        }
        let next_consecutive_failures = if intent.last_failure_class == failure_class {
            intent.consecutive_failures.saturating_add(1)
        } else {
            1
        };
        let delay =
            Store::session_pr_verification_retry_delay(next_consecutive_failures.saturating_sub(1));
        if failure_class == PR_VERIFICATION_FAILURE_AUTH {
            delay
                .saturating_mul(SESSION_PR_AUTH_RETRY_SECONDS)
                .min(6 * 60 * 60)
        } else {
            delay
        }
    }

    fn session_pr_verification_lock(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.session_pr_verification_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(session_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(session_id.to_string(), Arc::downgrade(&lock));
        lock
    }

    fn github_for_pr_verification(
        &self,
        intent: &SessionPrVerificationIntent,
    ) -> Result<crate::github::GitHub, EngineError> {
        let token = self.github_token(&intent.host).ok_or_else(|| {
            EngineError::BadRequest(format!(
                "no GitHub OAuth session for {}; sign in under Settings → Integrations",
                intent.host
            ))
        })?;
        crate::github::GitHub::new(&token, &intent.host, &intent.owner, &intent.repository)
            .map_err(EngineError::Internal)
    }

    fn pr_repository_and_branch_match(
        fetched: &crate::github::PullRequestWithHeadRepository,
        intent: &SessionPrVerificationIntent,
    ) -> bool {
        let expected_repository = format!("{}/{}", intent.owner, intent.repository);
        fetched
            .head_repository
            .as_deref()
            .is_some_and(|repository| repository.eq_ignore_ascii_case(&expected_repository))
            && fetched
                .info
                .repository
                .eq_ignore_ascii_case(&expected_repository)
            && fetched.info.head == intent.branch
    }

    fn pr_matches_verification_intent(
        fetched: &crate::github::PullRequestWithHeadRepository,
        intent: &SessionPrVerificationIntent,
    ) -> bool {
        Self::pr_repository_and_branch_match(fetched, intent)
            && fetched
                .info
                .head_sha
                .as_deref()
                .is_some_and(|sha| sha.eq_ignore_ascii_case(&intent.head_sha))
    }

    /// Accept a PR whose head advanced only when this exact session worktree
    /// now has the same branch and fetched tip checked out and the immutable
    /// completion-time commit is its ancestor. Remote-only movement remains
    /// untrusted and retryable.
    async fn pr_matches_advanced_session_head(
        session: &Session,
        fetched: &crate::github::PullRequestWithHeadRepository,
        intent: &SessionPrVerificationIntent,
    ) -> bool {
        let Some(head_sha) = fetched.info.head_sha.clone() else {
            return false;
        };
        let worktree = PathBuf::from(&session.worktree_path);
        let branch = intent.branch.clone();
        let ancestor = intent.head_sha.clone();
        tokio::task::spawn_blocking(move || {
            crate::git::checked_out_branch_descends_from(&worktree, &branch, &head_sha, &ancestor)
        })
        .await
        .unwrap_or(false)
    }

    async fn reconcile_session_pr_verifications(&self, session_id: &str) {
        let verification_lock = self.session_pr_verification_lock(session_id);
        let _verification_guard = verification_lock.lock().await;

        if self.deleting_sessions.lock().unwrap().contains(session_id) {
            return;
        }
        let lifecycle = self.session_lock(session_id);
        let (intents, mut recorded) = {
            let _lifecycle_guard = lifecycle.read().await;
            if self.deleting_sessions.lock().unwrap().contains(session_id) {
                return;
            }
            let Ok(Some(_)) = self.store.session(session_id) else {
                return;
            };
            let intents = match self.store.due_session_pr_verification_intents(
                session_id,
                MAX_SESSION_PR_VERIFICATIONS_PER_PASS,
            ) {
                Ok(intents) => intents,
                Err(error) => {
                    tracing::warn!(session_id, %error, "cannot load PR verification intents");
                    return;
                }
            };
            let recorded = match self.recorded_session_pr_numbers(session_id) {
                Ok(recorded) => recorded,
                Err(error) => {
                    tracing::warn!(session_id, %error, "cannot load recorded pull requests");
                    return;
                }
            };
            (intents, recorded)
        };
        for mut intent in intents {
            if recorded.contains(&intent.number) {
                self.discard_session_pr_verification_with_backoff(
                    &intent,
                    "discarding already-recorded pull request verification",
                );
                continue;
            }
            if Self::session_pr_verification_expired(&intent) {
                self.discard_session_pr_verification_with_backoff(
                    &intent,
                    "discarding expired pull request verification",
                );
                continue;
            }
            // Compatibility for rows written by the short-lived
            // pending-evidence format. Bound the legacy migration wait and
            // hold the lifecycle guard so restore/deletion cannot tear down
            // the worktree while its evidence is upgraded.
            if intent.branch.is_empty() || intent.head_sha.is_empty() {
                let _lifecycle_guard = lifecycle.read().await;
                if self.deleting_sessions.lock().unwrap().contains(session_id) {
                    return;
                }
                let Ok(Some(session)) = self.store.session(session_id) else {
                    return;
                };
                let lane = self.tool_execution_lock(session_id);
                let Ok(permit) = tokio::time::timeout(
                    SESSION_PR_LEGACY_EVIDENCE_LOCK_TIMEOUT,
                    lane.write_owned(),
                )
                .await
                else {
                    self.defer_or_expire_session_pr_verification(
                        &intent,
                        PR_VERIFICATION_FAILURE_CONTENTION,
                        None,
                        false,
                    );
                    continue;
                };
                let evidence = Self::capture_session_pr_head(&session).await;
                drop(permit);
                let Some((branch, head_sha)) = evidence else {
                    self.defer_or_expire_session_pr_verification(
                        &intent,
                        PR_VERIFICATION_FAILURE_EVIDENCE,
                        Some(MAX_SESSION_PR_NOT_FOUND_ATTEMPTS),
                        false,
                    );
                    continue;
                };
                match self
                    .store
                    .set_session_pr_verification_evidence(&intent, &branch, &head_sha)
                {
                    Ok(true) => {
                        intent.branch = branch;
                        intent.head_sha = head_sha;
                        intent.attempts = 0;
                        intent.last_failure_class.clear();
                        intent.consecutive_failures = 0;
                    }
                    Ok(false) => continue,
                    Err(error) => {
                        tracing::warn!(
                            session_id,
                            pr_number = intent.number,
                            %error,
                            "cannot upgrade legacy pull request ownership evidence"
                        );
                        self.defer_or_expire_session_pr_verification(
                            &intent,
                            PR_VERIFICATION_FAILURE_EVIDENCE,
                            Some(MAX_SESSION_PR_NOT_FOUND_ATTEMPTS),
                            false,
                        );
                        continue;
                    }
                }
            }
            let github = match self.github_for_pr_verification(&intent) {
                Ok(github) => github,
                Err(error) => {
                    tracing::warn!(session_id, error = %error, "cannot authenticate PR verification");
                    self.defer_or_expire_session_pr_verification(
                        &intent,
                        PR_VERIFICATION_FAILURE_AUTH,
                        None,
                        false,
                    );
                    continue;
                }
            };
            let fetched = tokio::time::timeout(
                Duration::from_secs(15),
                github.pr_with_head_repository(intent.number),
            )
            .await;
            match fetched {
                Ok(Ok(Some(fetched))) => {
                    // Remote I/O above deliberately runs without the session
                    // lifecycle guard. Reacquire it before touching the
                    // worktree or committing session-local durable state.
                    let _lifecycle_guard = lifecycle.read().await;
                    if self.deleting_sessions.lock().unwrap().contains(session_id) {
                        return;
                    }
                    let Ok(Some(session)) = self.store.session(session_id) else {
                        return;
                    };
                    if intent.branch != session.branch {
                        self.discard_session_pr_verification_with_backoff(
                            &intent,
                            "discarding pull request verification for a non-session branch",
                        );
                        continue;
                    }
                    let repository_and_branch_match =
                        Self::pr_repository_and_branch_match(&fetched, &intent);
                    let verified = Self::pr_matches_verification_intent(&fetched, &intent)
                        || (repository_and_branch_match
                            && Self::pr_matches_advanced_session_head(&session, &fetched, &intent)
                                .await);
                    if !verified {
                        if repository_and_branch_match {
                            tracing::debug!(
                                session_id,
                                pr_number = intent.number,
                                pr_head = fetched.info.head,
                                "deferring pull request whose head moved without matching the session worktree"
                            );
                            self.defer_or_expire_session_pr_verification(
                                &intent,
                                PR_VERIFICATION_FAILURE_HEAD_MOVED,
                                Some(MAX_SESSION_PR_HEAD_MOVED_ATTEMPTS),
                                true,
                            );
                        } else {
                            tracing::warn!(
                                session_id,
                                pr_number = intent.number,
                                pr_head = fetched.info.head,
                                "discarding pull request that does not match session-owned repository and branch evidence"
                            );
                            self.discard_session_pr_verification_with_backoff(
                                &intent,
                                "discarding mismatched pull request verification",
                            );
                        }
                        continue;
                    }
                    let event = Event::SessionPrOpened {
                        number: intent.number,
                        url: crate::github::pr_url(
                            &intent.host,
                            &intent.owner,
                            &intent.repository,
                            intent.number,
                        ),
                    };
                    match self
                        .store
                        .complete_session_pr_verification(intent.clone(), event)
                        .await
                    {
                        Ok(Some(_)) => {
                            recorded.insert(intent.number);
                        }
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(
                                session_id,
                                pr_number = intent.number,
                                %error,
                                "cannot complete verified pull request association"
                            );
                            self.defer_or_expire_session_pr_verification(
                                &intent,
                                PR_VERIFICATION_FAILURE_PERSISTENCE,
                                None,
                                false,
                            );
                        }
                    }
                }
                Ok(Ok(None)) => {
                    tracing::debug!(
                        session_id,
                        pr_number = intent.number,
                        "deferring nullable pull request verification response"
                    );
                    self.defer_or_expire_session_pr_verification(
                        &intent,
                        PR_VERIFICATION_FAILURE_NOT_FOUND,
                        Some(MAX_SESSION_PR_NOT_FOUND_ATTEMPTS),
                        true,
                    );
                }
                Ok(Err(error)) => {
                    tracing::warn!(
                        session_id,
                        pr_number = intent.number,
                        %error,
                        "deferring unavailable pull request verification"
                    );
                    if github_error_requires_reauthentication(&error) {
                        self.defer_or_expire_session_pr_verification(
                            &intent,
                            PR_VERIFICATION_FAILURE_AUTH,
                            None,
                            false,
                        );
                    } else {
                        self.defer_or_expire_session_pr_verification(
                            &intent,
                            PR_VERIFICATION_FAILURE_TRANSIENT,
                            None,
                            true,
                        );
                    }
                }
                Err(_) => {
                    tracing::warn!(
                        session_id,
                        pr_number = intent.number,
                        "deferring timed-out pull request verification"
                    );
                    self.defer_or_expire_session_pr_verification(
                        &intent,
                        PR_VERIFICATION_FAILURE_TRANSIENT,
                        None,
                        true,
                    );
                }
            }
        }
    }

    fn defer_or_expire_session_pr_verification(
        &self,
        intent: &SessionPrVerificationIntent,
        failure_class: &str,
        max_consecutive_failures: Option<u32>,
        count_request: bool,
    ) {
        if Self::session_pr_verification_expired(intent)
            || Self::session_pr_verification_retry_exhausted(
                intent,
                failure_class,
                max_consecutive_failures,
                count_request,
            )
        {
            self.discard_session_pr_verification_with_backoff(
                intent,
                "discarding exhausted pull request verification",
            );
        } else {
            let delay =
                Self::session_pr_verification_retry_delay(intent, failure_class, count_request);
            if let Err(error) = self.store.defer_session_pr_verification(
                intent,
                failure_class,
                count_request,
                delay,
            ) {
                tracing::warn!(
                    session_id = intent.session_id,
                    pr_number = intent.number,
                    %error,
                    "cannot update pull request verification retry state"
                );
            }
        }
    }

    fn discard_session_pr_verification_with_backoff(
        &self,
        intent: &SessionPrVerificationIntent,
        operation: &'static str,
    ) {
        if let Err(error) = self.store.discard_session_pr_verification(intent) {
            tracing::warn!(
                session_id = intent.session_id,
                pr_number = intent.number,
                %error,
                operation,
            );
            let delay = Self::session_pr_verification_retry_delay(
                intent,
                PR_VERIFICATION_FAILURE_PERSISTENCE,
                false,
            );
            if let Err(defer_error) = self.store.defer_session_pr_verification(
                intent,
                PR_VERIFICATION_FAILURE_PERSISTENCE,
                false,
                delay,
            ) {
                tracing::warn!(
                    session_id = intent.session_id,
                    pr_number = intent.number,
                    %defer_error,
                    "cannot defer a pull request verification after discard failed"
                );
            }
        }
    }

    pub async fn retry_session_pr_verifications(&self) {
        let sessions = match self.store.due_session_pr_verification_sessions(100) {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "cannot list durable PR verification intents");
                return;
            }
        };
        futures::stream::iter(sessions)
            .for_each_concurrent(
                MAX_PARALLEL_SESSION_PR_VERIFICATIONS,
                |session_id| async move {
                    self.reconcile_session_pr_verifications(&session_id).await;
                },
            )
            .await;
    }

    pub fn start_session_pr_verification_worker(self: &Arc<Self>) {
        if self
            .session_pr_verification_worker_started
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let engine = Arc::downgrade(self);
        let wake = Arc::clone(&self.session_pr_verification_wake);
        tokio::spawn(async move {
            loop {
                let Some(engine) = engine.upgrade() else {
                    break;
                };
                engine.retry_session_pr_verifications().await;
                drop(engine);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = wake.notified() => {}
                }
            }
        });
    }

    /// PR numbers already linked through persisted session events.
    fn recorded_session_pr_numbers(&self, session_id: &str) -> Result<HashSet<u64>, EngineError> {
        Ok(self
            .store
            .events_after(&Scope::Session(session_id.to_string()), 0)?
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::SessionPrOpened { number, .. } => Some(number),
                _ => None,
            })
            .collect())
    }

    /// Browser URLs already linked through persisted session events. Unlike
    /// `session_prs`, this never contacts GitHub and is therefore safe to fold
    /// for every session during client bootstrap.
    fn recorded_session_pr_urls(&self, session_id: &str) -> Result<HashSet<String>, EngineError> {
        Ok(self
            .store
            .events_after(&Scope::Session(session_id.to_string()), 0)?
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::SessionPrOpened { url, .. } => {
                    Some(url.trim_end_matches('/').to_ascii_lowercase())
                }
                _ => None,
            })
            .collect())
    }

    /// Provider-neutral evidence tying GitHub activity to this session.
    /// Explicit PR references work for any integration; successful tool args
    /// and produced commit IDs preserve enough identity to discover a PR that
    /// the user creates later in GitHub's UI.
    fn session_pr_evidence(
        &self,
        session_id: &str,
        host: &str,
        owner: &str,
        repo: &str,
    ) -> Result<SessionPrEvidence, EngineError> {
        let mut evidence = SessionPrEvidence::default();
        for envelope in self
            .store
            .events_after(&Scope::Session(session_id.to_string()), 0)?
        {
            match envelope.event {
                Event::SessionPrOpened { number, .. } => {
                    evidence.numbers.insert(number);
                    evidence.recorded_numbers.insert(number);
                }
                Event::CheckpointCreated { commit, .. } => {
                    evidence.commit_ids.insert(commit.to_ascii_lowercase());
                }
                _ => {}
            }
        }

        for thread in self.store.list_threads(session_id)? {
            let events = self.store.events_after(&Scope::Thread(thread.id), 0)?;
            evidence.extend(pr_evidence_from_events(
                events.into_iter().map(|envelope| envelope.event),
                host,
                owner,
                repo,
            ));
        }
        Ok(evidence)
    }

    /// The open PR associated with this session, if one exists.
    pub async fn session_pr(
        &self,
        session_id: &str,
    ) -> Result<Option<trouve_protocol::PrInfo>, EngineError> {
        Ok(self
            .session_prs(session_id)
            .await?
            .into_iter()
            .find(|pr| pr.state == "open"))
    }

    /// Every PR associated with the session (open first, newest first).
    ///
    /// This includes PRs from the worktree branch, explicitly referenced PRs,
    /// and open PRs whose head branch or commit appears in session activity.
    pub async fn session_prs(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::PrInfo>, EngineError> {
        let session = self.get_session(session_id)?;
        let repository = self.github_repository_for_session(&session)?;
        let (host, owner, repo) = &repository;
        let github = self.github_for_session(&session)?;
        let mut evidence = self.session_pr_evidence(session_id, host, owner, repo)?;
        let mut prs = github
            .prs_for_branch(&session.branch)
            .await
            .map_err(github_engine_error)?;
        let mut seen: HashSet<u64> = prs.iter().map(|pr| pr.number).collect();
        for pr in github
            .open_prs_referenced_by(&evidence.successful_tool_args, &evidence.commit_ids)
            .await
            .map_err(github_engine_error)?
        {
            if !self.pr_has_locally_verified_head(session_id, &pr).await {
                continue;
            }
            self.record_session_pr_numbers(
                session_id,
                &repository,
                [pr.number],
                &mut evidence.recorded_numbers,
            )?;
            if seen.insert(pr.number) {
                prs.push(pr);
            }
        }
        for number in evidence.numbers {
            if seen.insert(number) {
                let already_recorded = evidence.recorded_numbers.contains(&number);
                match github.pr(number).await {
                    Ok(pr)
                        if already_recorded
                            || self.pr_has_locally_verified_head(session_id, &pr).await =>
                    {
                        if !already_recorded {
                            self.record_session_pr_numbers(
                                session_id,
                                &repository,
                                [number],
                                &mut evidence.recorded_numbers,
                            )?;
                        }
                        prs.push(pr);
                    }
                    Ok(pr) => tracing::warn!(
                        session_id,
                        pr_number = number,
                        pr_head = pr.head,
                        "skipping linked pull request without matching session evidence"
                    ),
                    Err(error) => tracing::warn!(
                        session_id,
                        pr_number = number,
                        error = %error,
                        "skipping unavailable linked pull request"
                    ),
                }
            }
        }
        prs.sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
        Ok(prs)
    }

    /// Session-to-PR authorization from the newest persisted account
    /// snapshots. Unlike discovery (`session_prs`), this never contacts
    /// GitHub; the shell's 30-second account refresh keeps the projection
    /// current in the background.
    fn projected_session_prs(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::PrInfo>, EngineError> {
        let session = self.get_session(session_id)?;
        let linked_urls = self.recorded_session_pr_urls(session_id)?;
        let mut seen = HashSet::new();
        let mut prs = Vec::new();
        for (host, _) in self.github_hosts() {
            let Some(snapshot) = self.store.latest_github_pr_snapshot(&host)? else {
                continue;
            };
            prs.extend(snapshot.prs.into_iter().filter(|pr| {
                ((pr.workspace_id == session.workspace_id && pr.head == session.branch)
                    || linked_urls.contains(&pr.url.trim_end_matches('/').to_ascii_lowercase()))
                    && seen.insert((
                        pr.host.to_ascii_lowercase(),
                        pr.repository.to_ascii_lowercase(),
                        pr.number,
                    ))
            }));
        }
        prs.sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
        Ok(prs)
    }

    fn projected_session_pr(
        &self,
        session_id: &str,
        number: u64,
    ) -> Result<(Session, trouve_protocol::PrInfo), EngineError> {
        let session = self.get_session(session_id)?;
        let pr = self
            .projected_session_prs(session_id)?
            .into_iter()
            .find(|pr| pr.number == number)
            .ok_or_else(|| {
                EngineError::NotFound(format!(
                    "pull request #{number} is not associated with session {session_id}"
                ))
            })?;
        Ok((session, pr))
    }

    async fn cached_session_pr_detail(
        &self,
        session: &Session,
        pr: &trouve_protocol::PrInfo,
        sections: &HashSet<trouve_protocol::PrDetailSection>,
    ) -> Result<trouve_protocol::PrDetail, EngineError> {
        let key = GithubPrDetailKey::from_info(pr);
        if let Some(detail) = self
            .github_pr_detail_cache
            .lock()
            .unwrap()
            .get(&key, sections)
        {
            return Ok(detail);
        }
        let loaded = self
            .github_pr_detail_cache
            .lock()
            .unwrap()
            .loaded_sections(&key);
        let existing = self.github_pr_detail_cache.lock().unwrap().detail(&key);
        let mut missing = sections
            .difference(&loaded)
            .copied()
            .collect::<HashSet<_>>();
        if loaded.is_empty() {
            missing.insert(trouve_protocol::PrDetailSection::Overview);
        }
        let mut detail = self
            .github_for_session(session)?
            .pr_detail(pr.number, &missing, existing)
            .await
            .map_err(github_engine_error)?;
        detail.info.workspace_id = pr.workspace_id.clone();
        detail.info.trouve_review = pr.trouve_review.clone();
        Ok(self
            .github_pr_detail_cache
            .lock()
            .unwrap()
            .merge(&key, detail, missing))
    }

    /// Full GitHub collaboration state for one pull request already
    /// associated with this session. The association check prevents a
    /// session-scoped route from becoming a repository-wide PR browser.
    pub async fn session_pr_detail(
        &self,
        session_id: &str,
        number: u64,
        section: Option<trouve_protocol::PrDetailSection>,
    ) -> Result<trouve_protocol::PrDetail, EngineError> {
        let (session, pr) = self.projected_session_pr(session_id, number)?;
        let sections = section.map_or_else(
            || {
                HashSet::from([
                    trouve_protocol::PrDetailSection::Overview,
                    trouve_protocol::PrDetailSection::Conversation,
                    trouve_protocol::PrDetailSection::Commits,
                    trouve_protocol::PrDetailSection::Files,
                ])
            },
            |section| HashSet::from([trouve_protocol::PrDetailSection::Overview, section]),
        );
        self.cached_session_pr_detail(&session, &pr, &sections)
            .await
    }

    /// Bounded before/after content for one changed file in an associated
    /// session PR. The GitHub layer independently verifies that `path` belongs
    /// to the selected pull request before resolving either immutable blob.
    pub async fn session_pr_file_diff(
        &self,
        session_id: &str,
        number: u64,
        path: &str,
    ) -> Result<trouve_protocol::PrFileDiff, EngineError> {
        let (session, pr) = self.projected_session_pr(session_id, number)?;
        let detail = self
            .cached_session_pr_detail(
                &session,
                &pr,
                &HashSet::from([
                    trouve_protocol::PrDetailSection::Overview,
                    trouve_protocol::PrDetailSection::Files,
                ]),
            )
            .await?;
        let file = detail
            .files
            .iter()
            .find(|file| file.path == path)
            .ok_or_else(|| {
                EngineError::NotFound(format!(
                    "{path} is not a changed file in pull request #{number}"
                ))
            })?;
        let github = self.github_for_session(&session)?;
        match (detail.base_sha.as_deref(), detail.info.head_sha.as_deref()) {
            (Some(base), Some(head)) if !base.is_empty() && !head.is_empty() => github
                .pr_file_diff_known(file, base, head)
                .await
                .map_err(github_engine_error),
            _ => github
                .pr_file_diff(number, path)
                .await
                .map_err(github_engine_error),
        }
    }

    /// Apply a typed action to one associated pull request, then refresh the
    /// durable account projection used by the dashboard, session badges, and
    /// every open PR pane. A successful GitHub mutation is not reported as a
    /// failure merely because another configured host failed its refresh.
    pub async fn act_on_session_pr(
        &self,
        session_id: &str,
        number: u64,
        action: &trouve_protocol::PrActionRequest,
    ) -> Result<trouve_protocol::PrDetail, EngineError> {
        use trouve_protocol::{PrActionRequest as Action, PrDetailSection as Section};

        let (session, pr) = self.projected_session_pr(session_id, number)?;
        let key = GithubPrDetailKey::from_info(&pr);
        let required = match action {
            Action::UpdateReview { .. }
            | Action::DeleteReview { .. }
            | Action::DismissReview { .. }
            | Action::AddComment { .. }
            | Action::UpdateComment { .. }
            | Action::DeleteComment { .. }
            | Action::ReplyReviewThread { .. }
            | Action::ResolveReviewThread { .. }
            | Action::AddReaction { .. }
            | Action::RemoveReaction { .. } => {
                HashSet::from([Section::Overview, Section::Conversation])
            }
            Action::AddReviewThread { .. } | Action::SetFileViewed { .. } => {
                HashSet::from([Section::Overview, Section::Files])
            }
            _ => HashSet::from([Section::Overview]),
        };
        let detail = self
            .cached_session_pr_detail(&session, &pr, &required)
            .await?;
        self.github_for_session(&session)?
            .act_on_pr(&detail, action)
            .await
            .map_err(github_engine_error)?;

        let mut stale = HashSet::from([Section::Overview]);
        match action {
            Action::SubmitReview { .. }
            | Action::UpdateReview { .. }
            | Action::DeleteReview { .. }
            | Action::DismissReview { .. }
            | Action::AddComment { .. }
            | Action::UpdateComment { .. }
            | Action::DeleteComment { .. }
            | Action::ReplyReviewThread { .. }
            | Action::ResolveReviewThread { .. }
            | Action::AddReaction { .. }
            | Action::RemoveReaction { .. } => {
                stale.insert(Section::Conversation);
            }
            Action::AddReviewThread { .. } => {
                stale.insert(Section::Conversation);
                stale.insert(Section::Files);
            }
            Action::SetFileViewed { .. } => {
                stale.insert(Section::Files);
            }
            Action::UpdateBranch { .. } => {
                stale.insert(Section::Commits);
                stale.insert(Section::Files);
            }
            _ => {}
        }
        let mut refresh_sections = self
            .github_pr_detail_cache
            .lock()
            .unwrap()
            .loaded_sections(&key);
        if matches!(action, Action::UpdateBranch { .. }) {
            // A new head invalidates every cached connection, not only files
            // and commits. Re-fetch only the tabs this client population had
            // actually loaded, but never carry old-head data into the new key.
            stale.extend(refresh_sections.iter().copied());
        }
        refresh_sections.extend(stale.iter().copied());
        self.github_pr_detail_cache
            .lock()
            .unwrap()
            .mark_stale(&key, &stale);
        let refreshed = self
            .cached_session_pr_detail(&session, &pr, &refresh_sections)
            .await?;
        self.publish_github_pr_summary(&refreshed.info)?;
        Ok(refreshed)
    }

    /// Publish a mutation's already-returned summary into the durable account
    /// projection. This updates all open clients immediately without another
    /// GitHub discovery pass; the regular background refresh remains the
    /// authority for subsequent external changes.
    fn publish_github_pr_summary(&self, info: &trouve_protocol::PrInfo) -> Result<(), EngineError> {
        let _publication = self.github_dashboard_publication.lock().unwrap();
        let mut snapshot = self.store.latest_github_pr_snapshot(&info.host)?.unwrap_or(
            trouve_protocol::GithubPrList {
                viewer: String::new(),
                host: info.host.clone(),
                prs: Vec::new(),
            },
        );
        if let Some(existing) = snapshot.prs.iter_mut().find(|candidate| {
            candidate.number == info.number
                && candidate.repository.eq_ignore_ascii_case(&info.repository)
        }) {
            *existing = info.clone();
        } else {
            snapshot.prs.push(info.clone());
            snapshot
                .prs
                .sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
        }
        self.store.append_event(
            Scope::Server,
            Event::GithubPullRequestsUpdated {
                pull_requests: snapshot,
            },
        )?;
        Ok(())
    }

    /// Current durable server-owned UI projections paired with a server
    /// cursor. This is intentionally local-only: account PR state comes from
    /// the newest persisted replacement event and session associations come
    /// from branch identity plus recorded `session.pr_opened` links.
    pub fn server_projection_snapshot(
        &self,
    ) -> Result<(u64, trouve_protocol::ServerProjection), EngineError> {
        // Capture the resume boundary before reading any projection data. A
        // concurrent event can then only make the returned data newer than
        // this cursor; it cannot be hidden behind a cursor that already
        // covers data we did not read.
        let cursor = self
            .store
            .latest_event_cursor(&trouve_protocol::Scope::Server)?;
        let mut github_pull_requests = Vec::new();
        for (host, _) in self.github_hosts() {
            let Some(envelope) = self.store.latest_github_pr_snapshot_event(&host)? else {
                continue;
            };
            let Event::GithubPullRequestsUpdated { pull_requests } = envelope.event else {
                continue;
            };
            github_pull_requests.push(trouve_protocol::GithubPrHostProjection {
                cursor: envelope.cursor,
                refreshed_at: envelope.ts,
                pull_requests,
            });
        }

        let account_prs = github_pull_requests
            .iter()
            .flat_map(|snapshot| snapshot.pull_requests.prs.iter())
            .collect::<Vec<_>>();
        let mut session_pull_requests = Vec::new();
        for session in self.list_sessions(None)? {
            let linked_urls = self.recorded_session_pr_urls(&session.id)?;
            let mut seen = HashSet::new();
            let mut prs = account_prs
                .iter()
                .copied()
                .filter(|pr| {
                    (pr.workspace_id == session.workspace_id && pr.head == session.branch)
                        || linked_urls.contains(&pr.url.trim_end_matches('/').to_ascii_lowercase())
                })
                .filter(|pr| {
                    seen.insert((
                        pr.host.to_ascii_lowercase(),
                        pr.repository.to_ascii_lowercase(),
                        pr.number,
                    ))
                })
                .cloned()
                .collect::<Vec<_>>();
            prs.sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
            if !prs.is_empty() {
                session_pull_requests.push(trouve_protocol::SessionPrProjection {
                    session_id: session.id,
                    prs,
                });
            }
        }

        let git_worktree_settings = self.git_worktree_settings();
        Ok((
            cursor,
            trouve_protocol::ServerProjection {
                github_pull_requests,
                session_pull_requests,
                git_worktree_settings,
            },
        ))
    }

    /// Path of the MCP config file for a scope; workspace scope requires
    /// a workspace id.
    fn mcp_config_path(
        &self,
        scope: &str,
        workspace_id: Option<&str>,
    ) -> Result<PathBuf, EngineError> {
        match scope {
            "user" => {
                let dir = self
                    .config_dir
                    .as_deref()
                    .ok_or_else(|| EngineError::BadRequest("no config dir available".into()))?;
                Ok(crate::mcp::user_config_path(dir))
            }
            "workspace" => {
                let id = workspace_id.ok_or_else(|| {
                    EngineError::BadRequest("workspace scope needs workspace_id".into())
                })?;
                let ws = self
                    .store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
                Ok(crate::mcp::workspace_config_path(Path::new(&ws.path)))
            }
            other => Err(EngineError::BadRequest(format!(
                "unknown MCP scope '{other}' (use \"user\" or \"workspace\")"
            ))),
        }
    }

    /// User-managed MCP servers: the config dir's `mcp.json` plus each
    /// workspace's `.agents/.mcp.json` (one workspace when an id is given,
    /// every registered workspace otherwise). With `probe`, every enabled
    /// server is spawned and handshaken concurrently to report health.
    pub async fn list_mcp_servers(
        &self,
        workspace_id: Option<&str>,
        probe: bool,
    ) -> Result<Vec<trouve_protocol::McpServerInfo>, EngineError> {
        // (name, scope, workspace id, workspace name, config)
        type Entry = (String, String, String, String, crate::mcp::McpServerConfig);
        let mut entries: Vec<Entry> = Vec::new();
        if let Some(dir) = self.config_dir.as_deref() {
            for (name, config) in crate::mcp::read_servers(&crate::mcp::user_config_path(dir)) {
                entries.push((name, "user".into(), String::new(), String::new(), config));
            }
        }
        let workspaces = match workspace_id {
            Some(id) => vec![
                self.store
                    .workspace(id)?
                    .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?,
            ],
            None => self.store.list_workspaces()?,
        };
        for ws in workspaces {
            let path = crate::mcp::workspace_config_path(Path::new(&ws.path));
            for (name, config) in crate::mcp::read_servers(&path) {
                entries.push((
                    name,
                    "workspace".into(),
                    ws.id.clone(),
                    ws.name.clone(),
                    config,
                ));
            }
        }
        let probes = futures::future::join_all(entries.iter().map(
            |(name, scope, _, _, config)| async move {
                // Only probe (spawn) user-scope servers: workspace-scope
                // servers live in a repo's .agents/.mcp.json and are never
                // auto-run, so opening settings must not execute them.
                if probe && !config.disabled && scope == "user" {
                    Some(crate::mcp::probe(name, config, &self.mcp_logs).await)
                } else {
                    None
                }
            },
        ))
        .await;
        Ok(entries
            .into_iter()
            .zip(probes)
            .map(
                |((name, scope, workspace_id, workspace_name, config), probed)| {
                    let (health, detail) = if config.disabled {
                        ("disabled".to_string(), "disabled in this scope".to_string())
                    } else if scope == "workspace" {
                        (
                            "untrusted".to_string(),
                            "defined in this repo's .agents/.mcp.json; not auto-run. \
                             Copy it into your own config to trust and enable it."
                                .to_string(),
                        )
                    } else {
                        match probed {
                            Some(Ok(tools)) => ("ok".to_string(), format!("{tools} tools")),
                            Some(Err(e)) => ("error".to_string(), format!("{e:#}")),
                            None => self
                                .mcp_logs
                                .health(&name, &config)
                                .unwrap_or_else(|| ("unknown".to_string(), String::new())),
                        }
                    };
                    trouve_protocol::McpServerInfo {
                        name,
                        scope,
                        workspace_id,
                        workspace_name,
                        command: config.command,
                        args: config.args,
                        env: config.env,
                        enabled: Some(!config.disabled),
                        health,
                        detail,
                    }
                },
            )
            .collect())
    }

    /// The effective MCP config for one session: all scopes merged the way
    /// a turn in this session would see them (app-wide < workspace <
    /// branch), each entry tagged with the winning layer. Disabled entries
    /// are kept and flagged so tombstones are visible.
    pub fn session_mcp_servers(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::McpServerInfo>, EngineError> {
        let session = self.get_session(session_id)?;
        let workspace_root = self
            .store
            .workspace(&session.workspace_id)?
            .map(|ws| PathBuf::from(ws.path));
        Ok(crate::mcp::discover_with_provenance(
            self.config_dir.as_deref(),
            workspace_root.as_deref(),
            Path::new(&session.worktree_path),
        )
        .into_iter()
        .map(|(name, config, source)| {
            let (health, detail) = if config.disabled {
                (
                    "disabled".into(),
                    format!("disabled by the {source} config"),
                )
            } else if source != "app-wide" {
                (
                    "untrusted".into(),
                    "defined in this repository; copy it into the app-wide MCP settings to trust it"
                        .into(),
                )
            } else {
                self.mcp_logs
                    .health(&name, &config)
                    .unwrap_or_else(|| ("unknown".into(), "Health not checked".into()))
            };
            trouve_protocol::McpServerInfo {
                name,
                scope: source,
                workspace_id: session.workspace_id.clone(),
                workspace_name: String::new(),
                command: config.command,
                args: config.args,
                env: config.env,
                enabled: Some(!config.disabled),
                health,
                detail,
            }
        })
        .collect())
    }

    /// Add or replace an MCP server in the scope's config file.
    pub async fn upsert_mcp_server(
        &self,
        name: &str,
        req: &trouve_protocol::UpsertMcpServerRequest,
    ) -> Result<(), EngineError> {
        let name = name.trim();
        if name.is_empty() || name.contains("__") || name.contains('/') {
            return Err(EngineError::BadRequest(
                "server name must be non-empty and free of '__' and '/'".into(),
            ));
        }
        if req.command.trim().is_empty() {
            return Err(EngineError::BadRequest("command is required".into()));
        }
        let path = self.mcp_config_path(&req.scope, req.workspace_id.as_deref())?;
        let config = crate::mcp::McpServerConfig {
            command: req.command.trim().to_string(),
            args: req.args.clone(),
            env: req.env.clone(),
            disabled: !req.enabled.unwrap_or(true),
        };
        self.executor
            .mutate_mcp_config(&McpConfigMutationRequest {
                path,
                name: name.to_string(),
                mutation: McpConfigMutation::Upsert(config),
            })
            .await
            .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        Ok(())
    }

    /// Persistently enable or disable an existing MCP server without
    /// replacing the rest of its configuration.
    pub async fn set_mcp_server_enabled(
        &self,
        name: &str,
        req: &trouve_protocol::SetMcpServerEnabledRequest,
    ) -> Result<(), EngineError> {
        let name = name.trim();
        if name.is_empty() || name.contains("__") || name.contains('/') {
            return Err(EngineError::BadRequest(
                "server name must be non-empty and free of '__' and '/'".into(),
            ));
        }
        let path = self.mcp_config_path(&req.scope, req.workspace_id.as_deref())?;
        let outcome = self
            .executor
            .mutate_mcp_config(&McpConfigMutationRequest {
                path,
                name: name.to_string(),
                mutation: McpConfigMutation::SetEnabled(req.enabled),
            })
            .await
            .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        if outcome == McpConfigMutationOutcome::NotFound {
            return Err(EngineError::NotFound(format!("MCP server {name}")));
        }
        Ok(())
    }

    /// Remove an MCP server from the scope's config file.
    pub async fn delete_mcp_server(
        &self,
        name: &str,
        scope: &str,
        workspace_id: Option<&str>,
    ) -> Result<(), EngineError> {
        let path = self.mcp_config_path(scope, workspace_id)?;
        self.executor
            .mutate_mcp_config(&McpConfigMutationRequest {
                path,
                name: name.to_string(),
                mutation: McpConfigMutation::Remove,
            })
            .await
            .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        Ok(())
    }

    /// Recent log lines (stderr + lifecycle) for one MCP server.
    pub fn mcp_server_logs(&self, name: &str) -> trouve_protocol::McpLogs {
        trouve_protocol::McpLogs {
            lines: self.mcp_logs.lines(name),
        }
    }

    /// Subscription usage for every configured subscription provider.
    /// Codex answers via its app-server, Claude Code via its CLI's
    /// stream-json usage query, and Cursor via the dashboard's undocumented
    /// usage RPC (read with the CLI's stored login). Kimi Code uses the key
    /// stored for its provider preset against the same `/usages` endpoint as
    /// Kimi's open-source CLI.
    pub async fn subscription_health(&self) -> Vec<trouve_protocol::SubscriptionHealth> {
        let backends: Vec<(String, Arc<dyn AgentBackend>)> = {
            let map = self.backends.read().unwrap();
            let mut list: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            list.sort_by(|a, b| a.0.cmp(&b.0));
            list
        };
        let mut out = Vec::new();
        for (id, backend) in backends {
            match backend.subscription_health().await {
                Some(health) => out.push(health),
                None => out.push(trouve_protocol::SubscriptionHealth {
                    provider_id: id,
                    status: "unsupported".into(),
                    plan: String::new(),
                    windows: Vec::new(),
                    credits: String::new(),
                    note: "This vendor does not provide subscription usage to third-party apps."
                        .into(),
                }),
            }
        }
        let kimi_configs: Vec<(String, ProviderConfig)> = {
            let config = self.config.lock().unwrap();
            config
                .providers
                .iter()
                .filter(|(id, provider)| {
                    id.as_str() == "kimi-code"
                        && trouve_providers::kimi_usage::is_kimi_code_base_url(
                            provider.base_url.as_deref(),
                        )
                })
                .map(|(id, provider)| (id.clone(), provider.clone()))
                .collect()
        };
        for (id, provider) in kimi_configs {
            let Some(api_key) = resolved_api_key(&id, &provider, &self.secrets) else {
                out.push(trouve_protocol::SubscriptionHealth {
                    provider_id: id,
                    status: "unavailable".into(),
                    plan: String::new(),
                    windows: Vec::new(),
                    credits: String::new(),
                    note: "Kimi Code usage needs the subscription API key saved in Providers."
                        .into(),
                });
                continue;
            };
            let base_url = provider
                .base_url
                .as_deref()
                .unwrap_or(trouve_providers::kimi_usage::KIMI_CODE_BASE_URL);
            out.push(
                trouve_providers::kimi_usage::subscription_health(&id, base_url, &api_key).await,
            );
        }
        out.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        out
    }

    /// Whether GitHub calls can authenticate, per host. The top-level
    /// fields mirror github.com (`hosts[0]`) for older clients.
    pub fn github_integration(&self) -> trouve_protocol::GithubIntegration {
        let hosts: Vec<trouve_protocol::GithubHostIntegration> = self
            .github_hosts()
            .into_iter()
            .map(|(host, client_id)| {
                let configured = self.github_token(&host).is_some();
                trouve_protocol::GithubHostIntegration {
                    removable: host != crate::github::GITHUB_COM,
                    host,
                    configured,
                    source: if configured {
                        "oauth".into()
                    } else {
                        String::new()
                    },
                    oauth_available: client_id.is_some(),
                }
            })
            .collect();
        trouve_protocol::GithubIntegration {
            configured: hosts[0].configured,
            source: hosts[0].source.clone(),
            oauth_available: hosts[0].oauth_available,
            hosts,
        }
    }

    /// Register a self-hosted GitHub Enterprise instance so remotes on it
    /// resolve and it can hold its own auth.
    pub fn add_github_host(&self, host: &str, client_id: &str) -> Result<(), EngineError> {
        let host = host
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_ascii_lowercase();
        if host.is_empty() || !host.contains('.') || host.contains('/') || host.contains(':') {
            return Err(EngineError::BadRequest(
                "enter a bare hostname, e.g. github.example.com".into(),
            ));
        }
        if host == crate::github::GITHUB_COM {
            return Err(EngineError::BadRequest(
                "github.com is always available; add only enterprise hosts".into(),
            ));
        }
        let mut config = self.config.lock().unwrap();
        if config.github_enterprise.iter().any(|e| e.host == host) {
            return Err(EngineError::Conflict(format!("{host} is already added")));
        }
        if client_id.trim().is_empty() {
            return Err(EngineError::BadRequest(
                "an OAuth app client id is required for a GitHub Enterprise host".into(),
            ));
        }
        config
            .github_enterprise
            .push(crate::config::GithubEnterpriseConfig {
                host,
                client_id: Some(client_id.trim().to_string()),
            });
        let snapshot = config.clone();
        drop(config);
        self.persist_config(&snapshot);
        Ok(())
    }

    /// Remove an enterprise host and forget its stored secrets.
    pub fn remove_github_host(&self, host: &str) -> Result<(), EngineError> {
        let host = host.trim().to_ascii_lowercase();
        let snapshot = {
            let mut config = self.config.lock().unwrap();
            let before = config.github_enterprise.len();
            config.github_enterprise.retain(|e| e.host != host);
            if config.github_enterprise.len() == before {
                return Err(EngineError::NotFound(format!("GitHub host {host}")));
            }
            config.clone()
        };
        self.persist_config(&snapshot);
        let id = Self::github_secret_id(&host);
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::api_key_secret(&id));
        let _ = self
            .secrets
            .delete(&trouve_providers::secrets::oauth_secret(&id));
        {
            let _publication = self.github_dashboard_publication.lock().unwrap();
            {
                let mut dashboard_caches = self.github_dashboard_caches.lock().unwrap();
                dashboard_caches.remove(&host);
            }
            // The durable clear must stay ordered with stale-refresh
            // validation, not just with the in-memory cache removal.
            self.store.append_event(
                Scope::Server,
                Event::GithubPullRequestsUpdated {
                    pull_requests: trouve_protocol::GithubPrList {
                        viewer: String::new(),
                        host,
                        prs: Vec::new(),
                    },
                },
            )?;
        }
        Ok(())
    }

    /// Push the session branch and open a PR for it.
    pub async fn create_session_pr(
        &self,
        session_id: &str,
        req: &trouve_protocol::CreatePrRequest,
    ) -> Result<trouve_protocol::PrInfo, EngineError> {
        let _lifecycle = self.session_lock(session_id).read_owned().await;
        let _execution = self.tool_execution_lock(session_id).write_owned().await;
        let session = self
            .store
            .session(session_id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {session_id}")))?;
        let github = self.github_for_session(&session)?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_guard = cancel.clone().drop_guard();
        let base = self
            .executor
            .push_session_branch(&SessionRepositoryPush {
                managed_root: self.data_dir.join("worktrees"),
                worktree: PathBuf::from(&session.worktree_path),
                base_ref: session.base_ref.clone(),
                requested_base: req.base.clone(),
                branch: session.branch.clone(),
                cancel,
            })
            .await
            .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        drop(_execution);
        let mut pr = github
            .create_pr(&session.branch, &base, &req.title, &req.body, req.draft)
            .await
            .map_err(github_engine_error)?;
        pr.workspace_id = session.workspace_id.clone();
        self.store.append_event(
            Scope::Session(session.id.clone()),
            Event::SessionPrOpened {
                number: pr.number,
                url: pr.url.clone(),
            },
        )?;
        self.publish_github_pr_summary(&pr)?;
        Ok(pr)
    }

    /// Merge the session's PR.
    pub async fn merge_session_pr(
        &self,
        session_id: &str,
        method: Option<&str>,
    ) -> Result<(), EngineError> {
        let session = self.get_session(session_id)?;
        let github = self.github_for_session(&session)?;
        let pr = self
            .session_pr(session_id)
            .await?
            .ok_or_else(|| EngineError::NotFound("no open PR for this session".into()))?;
        github
            .merge_pr(pr.number, method.unwrap_or("merge"))
            .await
            .map_err(github_engine_error)
    }

    /// Unified diff of the session worktree against its base ref.
    pub async fn session_diff(&self, session_id: &str) -> Result<String, EngineError> {
        let _lifecycle = self.session_lock(session_id).read_owned().await;
        let _execution = self.tool_execution_lock(session_id).read_owned().await;
        let session = self
            .store
            .session(session_id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {session_id}")))?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_guard = cancel.clone().drop_guard();
        let request = SessionRepositoryDiff {
            managed_root: self.data_dir.join("worktrees"),
            worktree: PathBuf::from(&session.worktree_path),
            base_ref: session.base_ref,
            path: None,
            cancel,
        };
        self.executor
            .session_diff(&request)
            .await
            .map_err(session_diff_executor_error)
    }

    /// Return only bounded changed-path metadata. Patch text is loaded through
    /// `session_file_diff` after the client selects a file.
    pub async fn session_diff_summary(
        &self,
        session_id: &str,
    ) -> Result<SessionDiffSummary, EngineError> {
        let _lifecycle = self.session_lock(session_id).read_owned().await;
        let _execution = self.tool_execution_lock(session_id).read_owned().await;
        let session = self
            .store
            .session(session_id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {session_id}")))?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_guard = cancel.clone().drop_guard();
        let request = SessionRepositoryDiff {
            managed_root: self.data_dir.join("worktrees"),
            worktree: PathBuf::from(&session.worktree_path),
            base_ref: session.base_ref,
            path: None,
            cancel,
        };
        let stats = self
            .executor
            .session_diff_summary(&request)
            .await
            .map_err(session_diff_executor_error)?;
        let additions = stats.iter().map(|file| file.additions).sum();
        let deletions = stats.iter().map(|file| file.deletions).sum();
        Ok(SessionDiffSummary {
            additions,
            deletions,
            files: stats
                .into_iter()
                .map(|file| SessionDiffFileSummary {
                    path: file.path,
                    additions: file.additions,
                    deletions: file.deletions,
                    binary: file.binary,
                })
                .collect(),
        })
    }

    /// Return a bounded unified patch for one worktree-relative changed path.
    pub async fn session_file_diff(
        &self,
        session_id: &str,
        path: &str,
    ) -> Result<SessionFileDiff, EngineError> {
        let relative = Path::new(path);
        if path.is_empty()
            || relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir
                        | std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(EngineError::BadRequest(
                "diff path must be worktree-relative".into(),
            ));
        }
        let _lifecycle = self.session_lock(session_id).read_owned().await;
        let _execution = self.tool_execution_lock(session_id).read_owned().await;
        let session = self
            .store
            .session(session_id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {session_id}")))?;
        let selected_path = path.to_string();
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_guard = cancel.clone().drop_guard();
        let request = SessionRepositoryDiff {
            managed_root: self.data_dir.join("worktrees"),
            worktree: PathBuf::from(&session.worktree_path),
            base_ref: session.base_ref,
            path: Some(selected_path.clone()),
            cancel,
        };
        match self.executor.session_diff(&request).await {
            Ok(diff) => Ok(SessionFileDiff {
                path: selected_path,
                diff,
            }),
            Err(error) if error.contains("not a changed file") => Err(EngineError::NotFound(
                format!("path is not changed in this session: {path}"),
            )),
            Err(error) => Err(session_diff_executor_error(error)),
        }
    }

    /// List a directory inside the session worktree (IDE-style browsing).
    pub async fn session_list_dir(
        &self,
        session_id: &str,
        rel_path: &str,
    ) -> Result<Vec<trouve_protocol::DirEntry>, EngineError> {
        let session = self.get_session(session_id)?;
        let ctx = ToolCtx {
            worktree: PathBuf::from(&session.worktree_path),
            ..Default::default()
        };
        let full = ctx
            .resolve(rel_path)
            .map_err(|e| EngineError::BadRequest(e.to_string()))?;
        let mut rd = tokio::fs::read_dir(&full)
            .await
            .map_err(|e| EngineError::BadRequest(format!("cannot list {rel_path}: {e}")))?;
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push(trouve_protocol::DirEntry { name, is_dir });
        }
        entries.sort_by(|a, b| (b.is_dir, &a.name).cmp(&(a.is_dir, &b.name)));
        Ok(entries)
    }

    /// Every path in the session worktree (files, plus directories with a
    /// trailing '/'), worktree-relative, honouring .gitignore — feeds the
    /// composer's "@" file-mention completion. Capped; alphabetical.
    pub async fn session_list_paths(&self, session_id: &str) -> Result<Vec<String>, EngineError> {
        const MAX_PATHS: usize = 5000;
        let session = self.get_session(session_id)?;
        let worktree = PathBuf::from(&session.worktree_path);
        let paths = tokio::task::spawn_blocking(move || {
            let mut paths = Vec::new();
            let walker = ignore::WalkBuilder::new(&worktree)
                .hidden(true)
                .require_git(false)
                .build();
            for entry in walker.flatten() {
                let Ok(rel) = entry.path().strip_prefix(&worktree) else {
                    continue;
                };
                if rel.as_os_str().is_empty() {
                    continue;
                }
                let mut path = rel.to_string_lossy().replace('\\', "/");
                if entry.file_type().is_some_and(|t| t.is_dir()) {
                    path.push('/');
                }
                paths.push(path);
            }
            paths.sort();
            paths.truncate(MAX_PATHS);
            paths
        })
        .await
        .map_err(|e| EngineError::Internal(anyhow!("path walk failed: {e}")))?;
        Ok(paths)
    }

    /// Read a file inside the session worktree.
    pub async fn session_read_file(
        &self,
        session_id: &str,
        rel_path: &str,
    ) -> Result<String, EngineError> {
        let session = self.get_session(session_id)?;
        let ctx = ToolCtx {
            worktree: PathBuf::from(&session.worktree_path),
            ..Default::default()
        };
        let full = ctx
            .resolve(rel_path)
            .map_err(|e| EngineError::BadRequest(e.to_string()))?;
        tokio::fs::read_to_string(&full)
            .await
            .map_err(|e| EngineError::BadRequest(format!("cannot read {rel_path}: {e}")))
    }

    // --- integrated terminal --------------------------------------------

    fn ensure_terminal_session_available(
        &self,
        session_id: &str,
    ) -> Result<(Session, std::sync::MutexGuard<'_, HashSet<String>>), EngineError> {
        let (session, deleting) = self.ensure_terminal_session_exists(session_id)?;
        if session.archived {
            return Err(EngineError::Conflict(format!(
                "session {} is archived",
                session.id
            )));
        }
        if self.store.open_workspace(&session.workspace_id)?.is_none() {
            return Err(EngineError::Conflict(format!(
                "workspace {} is closed",
                session.workspace_id
            )));
        }
        Ok((session, deleting))
    }

    fn ensure_terminal_session_exists(
        &self,
        session_id: &str,
    ) -> Result<(Session, std::sync::MutexGuard<'_, HashSet<String>>), EngineError> {
        let deleting = self.deleting_sessions.lock().unwrap();
        if deleting.contains(session_id) {
            return Err(EngineError::Conflict(format!(
                "session {session_id} is being deleted"
            )));
        }
        // Re-read under the deletion admission lock. A caller may have loaded
        // the session before a concurrent delete committed and cleared its
        // transient marker.
        let session = self
            .store
            .session(session_id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {session_id}")))?;
        Ok((session, deleting))
    }

    /// The session's default interactive terminal, spawning a shell in its
    /// worktree if none is live. Ephemeral (not persisted, not in the event
    /// log). Kept for compatibility with the original singular endpoint.
    pub fn open_terminal(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<trouve_protocol::TerminalInfo, EngineError> {
        let (session, deleting) = self.ensure_terminal_session_available(session_id)?;
        let terminal = self
            .terminals
            .open_default(session_id, Path::new(&session.worktree_path), cols, rows)
            .map_err(EngineError::Internal)?;
        drop(deleting);
        Ok(terminal_info(&terminal))
    }

    /// All ephemeral terminals belonging to a session, in creation order.
    pub fn list_terminals(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::TerminalInfo>, EngineError> {
        // Listing is observational: archive/workspace close tears terminals
        // down, so callers should see an empty list while creation remains
        // rejected until the owner is reopened.
        let (_session, deleting) = self.ensure_terminal_session_exists(session_id)?;
        let terminals = self
            .terminals
            .list_session(session_id)
            .iter()
            .map(|terminal| terminal_info(terminal))
            .collect();
        drop(deleting);
        Ok(terminals)
    }

    /// Spawn another independent interactive terminal in the session worktree.
    pub fn create_terminal(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<trouve_protocol::TerminalInfo, EngineError> {
        let (session, deleting) = self.ensure_terminal_session_available(session_id)?;
        let terminal = self
            .terminals
            .create(session_id, Path::new(&session.worktree_path), cols, rows)
            .map_err(EngineError::Internal)?;
        drop(deleting);
        Ok(terminal_info(&terminal))
    }

    pub async fn terminal_input(&self, terminal_id: &str, bytes: &[u8]) -> Result<(), EngineError> {
        let terminal = self
            .terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        let operation = self.session_lock(&terminal.session_id).read_owned().await;
        if self.store.session(&terminal.session_id)?.is_none()
            || self.terminals.get(terminal_id).is_err()
        {
            return Err(EngineError::NotFound(format!("terminal {terminal_id}")));
        }
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            terminal.write(&bytes)
        })
        .await
        .map_err(|error| EngineError::Internal(anyhow!(error)))?
        .map_err(EngineError::Internal)
    }

    pub async fn terminal_resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), EngineError> {
        let terminal = self
            .terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        let operation = self.session_lock(&terminal.session_id).read_owned().await;
        if self.store.session(&terminal.session_id)?.is_none()
            || self.terminals.get(terminal_id).is_err()
        {
            return Err(EngineError::NotFound(format!("terminal {terminal_id}")));
        }
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            terminal.resize(cols, rows)
        })
        .await
        .map_err(|error| EngineError::Internal(anyhow!(error)))?
        .map_err(EngineError::Internal)
    }

    /// Kill one terminal without disturbing the session's other terminals.
    pub async fn terminal_kill(&self, terminal_id: &str) -> Result<(), EngineError> {
        // get() first so unknown ids surface as 404 rather than a no-op.
        let terminal = self
            .terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        let operation = self.session_lock(&terminal.session_id).read_owned().await;
        if self.store.session(&terminal.session_id)?.is_none()
            || self.terminals.get(terminal_id).is_err()
        {
            return Err(EngineError::NotFound(format!("terminal {terminal_id}")));
        }
        let terminals = self.terminals.clone();
        let terminal_id = terminal_id.to_string();
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            terminals.remove(&terminal_id);
        })
        .await
        .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        Ok(())
    }

    /// Attach to a terminal's output from byte offset `after`: the retained
    /// backlog from there plus a live receiver (empty chunk = shell exited).
    pub async fn terminal_subscribe(
        &self,
        terminal_id: &str,
        after: u64,
    ) -> Result<
        (
            u64,
            Vec<u8>,
            tokio::sync::broadcast::Receiver<bytes::Bytes>,
            bool,
        ),
        EngineError,
    > {
        let terminal = self
            .terminals
            .get(terminal_id)
            .map_err(|e| EngineError::NotFound(e.to_string()))?;
        let _operation = self.session_lock(&terminal.session_id).read_owned().await;
        if self.store.session(&terminal.session_id)?.is_none()
            || self.terminals.get(terminal_id).is_err()
        {
            return Err(EngineError::NotFound(format!("terminal {terminal_id}")));
        }
        let (from, replay, rx) = terminal.subscribe(after);
        Ok((from, replay, rx, terminal.exited()))
    }

    fn session_lock(&self, session_id: &str) -> Arc<tokio::sync::RwLock<()>> {
        self.session_locks
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .clone()
    }

    fn tool_execution_lock(&self, session_id: &str) -> Arc<tokio::sync::RwLock<()>> {
        let mut locks = self.tool_execution_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(session_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::RwLock::new(()));
        locks.insert(session_id.to_string(), Arc::downgrade(&lock));
        lock
    }

    // --- workspaces ---------------------------------------------------------

    fn resolve_workspace_list_item(workspace: Workspace, timeout: Duration) -> WorkspaceListItem {
        let repository = Path::new(&workspace.path);
        let (remote_url, common_directory) =
            git::workspace_repository_sources(repository, "origin", timeout, |remote| {
                crate::github::parse_remote(remote).is_some()
            });
        let remote_identity = remote_url.and_then(|remote| crate::github::parse_remote(&remote));
        let (repository_key, repository_name) = if let Some((host, owner, name)) = remote_identity {
            (
                format!(
                    "remote:{host}/{owner}/{name}",
                    host = host.to_ascii_lowercase(),
                    owner = owner.to_ascii_lowercase(),
                    name = name.to_ascii_lowercase(),
                ),
                name,
            )
        } else {
            (
                common_directory
                    .map(|directory| format!("local:{}", directory.to_string_lossy()))
                    .unwrap_or_else(|| format!("workspace:{}", workspace.id)),
                workspace.name.clone(),
            )
        };
        WorkspaceListItem {
            id: workspace.id,
            name: workspace.name,
            path: workspace.path,
            repository_key: Some(repository_key),
            repository_name: Some(repository_name),
        }
    }

    fn fallback_workspace_list_item(workspace: Workspace) -> WorkspaceListItem {
        WorkspaceListItem {
            repository_key: Some(format!("workspace:{}", workspace.id)),
            repository_name: Some(workspace.name.clone()),
            id: workspace.id,
            name: workspace.name,
            path: workspace.path,
        }
    }

    fn workspace_list_refresh_lock(&self, workspace_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.workspace_list_refresh_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(workspace_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(workspace_id.to_string(), Arc::downgrade(&lock));
        lock
    }

    fn cache_workspace_list_item(&self, item: WorkspaceListItem) -> WorkspaceListItem {
        self.workspace_list_cache.lock().unwrap().insert(
            item.id.clone(),
            WorkspaceListCacheEntry {
                item: item.clone(),
                refreshed_at: Instant::now(),
            },
        );
        item
    }

    fn cached_workspace_list_item(
        &self,
        workspace: Workspace,
        request_deadline: Instant,
    ) -> WorkspaceListItem {
        self.cached_workspace_list_item_with(
            workspace,
            request_deadline,
            Self::resolve_workspace_list_item,
        )
    }

    fn cached_workspace_list_item_with(
        &self,
        workspace: Workspace,
        request_deadline: Instant,
        resolve: impl FnOnce(Workspace, Duration) -> WorkspaceListItem,
    ) -> WorkspaceListItem {
        let cached = self
            .workspace_list_cache
            .lock()
            .unwrap()
            .get(&workspace.id)
            .map(|entry| (entry.item.clone(), entry.refreshed_at));
        if let Some((cached, refreshed_at)) = &cached
            && refreshed_at.elapsed() < WORKSPACE_LIST_CACHE_TTL
        {
            return cached.clone();
        }

        let refresh_lock = self.workspace_list_refresh_lock(&workspace.id);
        let Ok(_refresh) = refresh_lock.try_lock() else {
            return cached
                .map(|(item, _)| item)
                .unwrap_or_else(|| Self::fallback_workspace_list_item(workspace));
        };
        let cached = self
            .workspace_list_cache
            .lock()
            .unwrap()
            .get(&workspace.id)
            .map(|entry| (entry.item.clone(), entry.refreshed_at));
        if let Some((cached, refreshed_at)) = &cached
            && refreshed_at.elapsed() < WORKSPACE_LIST_CACHE_TTL
        {
            return cached.clone();
        }
        let Some(remaining) = request_deadline.checked_duration_since(Instant::now()) else {
            return cached
                .map(|(item, _)| item)
                .unwrap_or_else(|| Self::fallback_workspace_list_item(workspace));
        };
        self.cache_workspace_list_item(resolve(workspace, remaining))
    }

    fn canonical_workspace_registration_path(path: &str) -> Result<PathBuf, EngineError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| EngineError::BadRequest(format!("invalid path {path}: {e}")))?;
        Ok(canonical)
    }

    fn workspace_registration_lock(&self, canonical: &Path) -> Arc<Mutex<()>> {
        self.workspace_list_refresh_lock(&format!("registration:{}", canonical.to_string_lossy()))
    }

    fn acquire_workspace_registration_lock<'a>(
        registration_lock: &'a Mutex<()>,
        on_lock_attempt: impl FnOnce(bool),
    ) -> MutexGuard<'a, ()> {
        match registration_lock.try_lock() {
            Ok(guard) => {
                on_lock_attempt(false);
                guard
            }
            Err(TryLockError::WouldBlock) => {
                on_lock_attempt(true);
                registration_lock.lock().unwrap()
            }
            Err(TryLockError::Poisoned(error)) => panic!("{error}"),
        }
    }

    fn prepare_workspace_registration(
        &self,
        canonical: &Path,
        name: Option<String>,
    ) -> Result<(Workspace, bool), EngineError> {
        if !git::is_git_repo(canonical) {
            return Err(EngineError::BadRequest(format!(
                "{} is not a git repository",
                canonical.display()
            )));
        }
        let path_str = canonical.to_string_lossy().to_string();
        if let Some(existing) = self.store.workspace_by_path(&path_str)? {
            return Ok((existing, true));
        }
        let workspace = Workspace {
            id: new_id("ws"),
            name: name.unwrap_or_else(|| {
                canonical
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "workspace".into())
            }),
            path: path_str.clone(),
        };
        Ok((workspace, false))
    }

    fn commit_workspace_registration(
        &self,
        workspace: Workspace,
        item: WorkspaceListItem,
        existing: bool,
    ) -> Result<(WorkspaceListItem, bool), EngineError> {
        let mutated = if existing {
            if self.store.set_workspace_closed(&workspace.id, false)? {
                for session in self.store.list_sessions(Some(&workspace.id))? {
                    if !session.archived {
                        self.terminals.reopen_session(&session.id);
                    }
                }
                self.store.append_event(
                    Scope::Server,
                    Event::WorkspaceRegistered {
                        workspace_id: workspace.id.clone(),
                        path: workspace.path,
                    },
                )?;
                true
            } else {
                false
            }
        } else {
            self.store.insert_workspace(&workspace)?;
            self.store.append_event(
                Scope::Server,
                Event::WorkspaceRegistered {
                    workspace_id: workspace.id.clone(),
                    path: workspace.path,
                },
            )?;
            true
        };
        Ok((self.cache_workspace_list_item(item), mutated))
    }

    pub fn register_workspace(
        &self,
        path: &str,
        name: Option<String>,
    ) -> Result<WorkspaceListItem, EngineError> {
        self.register_workspace_with(path, name, |_| {}, || {})
    }

    fn register_workspace_with(
        &self,
        path: &str,
        name: Option<String>,
        on_lock_attempt: impl FnOnce(bool),
        after_prepare: impl FnOnce(),
    ) -> Result<WorkspaceListItem, EngineError> {
        let canonical = Self::canonical_workspace_registration_path(path)?;
        let registration_lock = self.workspace_registration_lock(&canonical);
        let _registration =
            Self::acquire_workspace_registration_lock(&registration_lock, on_lock_attempt);
        let (workspace, existing) = self.prepare_workspace_registration(&canonical, name)?;
        after_prepare();
        let refresh_lock = self.workspace_list_refresh_lock(&workspace.id);
        let _refresh = refresh_lock.lock().unwrap();
        let item = Self::resolve_workspace_list_item(
            workspace.clone(),
            WORKSPACE_REPOSITORY_IDENTITY_TIMEOUT,
        );
        self.commit_workspace_registration(workspace, item, existing)
            .map(|(item, _mutated)| item)
    }

    pub(crate) fn register_review_workspace(
        &self,
        path: &str,
        name: Option<String>,
        cancel: &tokio_util::sync::CancellationToken,
        commit_fence: &ReviewWorkspaceRegistrationFence,
    ) -> Result<WorkspaceListItem, EngineError> {
        self.register_review_workspace_with(path, name, cancel, commit_fence, || {}, || {})
    }

    pub(crate) fn cancel_review_workspace_registration(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
        commit_fence: &ReviewWorkspaceRegistrationFence,
    ) -> Result<(), EngineError> {
        cancel.cancel();
        let committed = commit_fence.committed.lock().unwrap().clone();
        if let Some(committed) = committed.filter(|committed| committed.mutated) {
            self.close_workspace(&committed.workspace_id)?;
        }
        Ok(())
    }

    pub(crate) fn complete_review_workspace_registration(
        &self,
        commit_fence: &ReviewWorkspaceRegistrationFence,
    ) {
        commit_fence.committed.lock().unwrap().take();
    }

    fn register_review_workspace_with(
        &self,
        path: &str,
        name: Option<String>,
        cancel: &tokio_util::sync::CancellationToken,
        commit_fence: &ReviewWorkspaceRegistrationFence,
        before_commit: impl FnOnce(),
        after_cancel_check: impl FnOnce(),
    ) -> Result<WorkspaceListItem, EngineError> {
        if cancel.is_cancelled() {
            return Err(EngineError::BadRequest(
                "stale: review workspace registration was cancelled".into(),
            ));
        }
        let canonical = Self::canonical_workspace_registration_path(path)?;
        let registration_lock = self.workspace_registration_lock(&canonical);
        let _registration = registration_lock.lock().unwrap();
        if cancel.is_cancelled() {
            return Err(EngineError::BadRequest(
                "stale: review workspace registration was cancelled".into(),
            ));
        }
        let (workspace, existing) = self.prepare_workspace_registration(&canonical, name)?;
        let refresh_lock = self.workspace_list_refresh_lock(&workspace.id);
        let _refresh = refresh_lock.lock().unwrap();
        let item = Self::resolve_workspace_list_item(
            workspace.clone(),
            WORKSPACE_REPOSITORY_IDENTITY_TIMEOUT,
        );
        before_commit();
        let mut committed = commit_fence.committed.lock().unwrap();
        if cancel.is_cancelled() {
            return Err(EngineError::BadRequest(
                "stale: review workspace registration was cancelled".into(),
            ));
        }
        after_cancel_check();
        let (item, mutated) = self.commit_workspace_registration(workspace, item, existing)?;
        *committed = Some(ReviewWorkspaceRegistrationCommit {
            workspace_id: item.id.clone(),
            mutated,
        });
        Ok(item)
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceListItem>, EngineError> {
        let request_deadline = Instant::now() + WORKSPACE_REPOSITORY_IDENTITY_TIMEOUT;
        Ok(self
            .store
            .list_workspaces()?
            .into_iter()
            .map(|workspace| self.cached_workspace_list_item(workspace, request_deadline))
            .collect())
    }

    /// Capture authenticated hosts and register their cache handles as one
    /// publication-locked lifecycle step.
    fn prepare_github_dashboard_refreshes(&self) -> Vec<GithubDashboardRefresh> {
        self.prepare_github_dashboard_refreshes_with(|| {})
    }

    /// Testable preparation path. `after_capture` runs with the publication
    /// lock held, after host/token capture and before cache pruning and
    /// registration, so tests can coordinate a concurrent removal.
    fn prepare_github_dashboard_refreshes_with(
        &self,
        after_capture: impl FnOnce(),
    ) -> Vec<GithubDashboardRefresh> {
        // Host/token capture and cache registration are one lifecycle step:
        // removal must either run before both or clear the registered handles
        // after both.
        let _publication = self.github_dashboard_publication.lock().unwrap();
        let authenticated_hosts = self
            .github_hosts()
            .into_iter()
            .filter_map(|(host, _)| self.github_token(&host).map(|token| (host, token)))
            .collect::<Vec<_>>();
        after_capture();
        let known_hosts = authenticated_hosts
            .iter()
            .map(|(host, _token)| host.clone())
            .collect::<HashSet<_>>();
        let mut dashboard_caches = self.github_dashboard_caches.lock().unwrap();
        dashboard_caches.retain(|host, _cache| known_hosts.contains(host));
        authenticated_hosts
            .into_iter()
            .map(|(host, token)| {
                let cache = dashboard_caches
                    .entry(host.clone())
                    .or_insert_with(|| {
                        Arc::new(tokio::sync::Mutex::new(
                            crate::github::GitHubDashboardCache::default(),
                        ))
                    })
                    .clone();
                (host, token, cache)
            })
            .collect()
    }

    /// Refresh the account-centric PR feed on every signed-in GitHub instance.
    pub async fn refresh_github_prs(&self, force: bool) -> Result<(), EngineError> {
        let request_started = Instant::now();
        let terminal_since = chrono::Utc::now() - chrono::Duration::hours(24);
        let workspaces = self.store.list_workspaces()?;
        let workspace_repositories = tokio::task::spawn_blocking(move || {
            workspaces
                .into_iter()
                .filter_map(|workspace| {
                    let remote = git::remote_url(Path::new(&workspace.path), "origin")?;
                    let (host, owner, repo) = crate::github::parse_remote(&remote)?;
                    Some((host, format!("{owner}/{repo}"), workspace.id))
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        let mut failures = Vec::new();
        let refreshes = self.prepare_github_dashboard_refreshes();
        for (host, token, cache_handle) in refreshes {
            let mut cache = if force {
                cache_handle.lock().await
            } else {
                let Ok(cache) = cache_handle.try_lock() else {
                    tracing::debug!(host, "coalescing concurrent GitHub dashboard refresh");
                    continue;
                };
                cache
            };
            if !cache.should_refresh(
                force,
                request_started,
                Instant::now(),
                GITHUB_DASHBOARD_REFRESH_FRESHNESS,
            ) {
                tracing::debug!(host, force, "reusing fresh GitHub dashboard snapshot");
                continue;
            }
            let account =
                crate::github::GitHubAccount::new(&token, &host).map_err(EngineError::Internal)?;
            let refresh = tokio::time::timeout(
                GITHUB_DASHBOARD_REFRESH_TIMEOUT,
                account.dashboard_prs(terminal_since, &mut cache),
            )
            .await;
            let (viewer, mut prs) = match refresh {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    failures.push(format!("{host}: {error:#}"));
                    continue;
                }
                Err(_) => {
                    failures.push(format!(
                        "{host}: GitHub dashboard refresh timed out after {}s",
                        GITHUB_DASHBOARD_REFRESH_TIMEOUT.as_secs()
                    ));
                    continue;
                }
            };
            let bot_login = self.github_app_status()?.bot_login;
            for pr in &mut prs {
                pr.workspace_id = workspace_repositories
                    .iter()
                    .find_map(|(host, repository, workspace_id)| {
                        (host == &pr.host && repository == &pr.repository)
                            .then(|| workspace_id.clone())
                    })
                    .unwrap_or_default();
                pr.trouve_review = self.store.latest_code_review_for_pull(
                    &pr.repository,
                    pr.number,
                    pr.head_sha.as_deref(),
                    &bot_login,
                )?;
            }
            let pull_requests = trouve_protocol::GithubPrList { viewer, host, prs };
            let persisted = self.store.latest_github_pr_snapshot(&pull_requests.host)?;
            if let Some(previous) = persisted.as_ref() {
                let mut detail_cache = self.github_pr_detail_cache.lock().unwrap();
                for pr in &pull_requests.prs {
                    let changed = previous
                        .prs
                        .iter()
                        .find(|candidate| {
                            candidate.number == pr.number
                                && candidate.repository.eq_ignore_ascii_case(&pr.repository)
                        })
                        .is_none_or(|candidate| {
                            serde_json::to_value(candidate).ok() != serde_json::to_value(pr).ok()
                        });
                    if changed {
                        detail_cache.invalidate_pr(pr);
                    }
                }
            }
            if !cache.has_published_snapshot()
                && let Some(persisted) = persisted.as_ref()
            {
                cache.seed_published_snapshot(persisted)?;
            }
            if let Some(snapshot) = cache.unpublished_snapshot(&pull_requests)? {
                let _publication = self.github_dashboard_publication.lock().unwrap();
                let cache_is_current = {
                    let dashboard_caches = self.github_dashboard_caches.lock().unwrap();
                    dashboard_caches
                        .get(&pull_requests.host)
                        .is_some_and(|current| Arc::ptr_eq(current, &cache_handle))
                };
                if !cache_is_current {
                    continue;
                }
                // Validation, durable publication, and cache marking must be
                // serialized together with host removal.
                self.store.append_event(
                    Scope::Server,
                    Event::GithubPullRequestsUpdated { pull_requests },
                )?;
                cache.mark_snapshot_published(snapshot);
            }
            cache.mark_refresh_completed();
        }
        if !failures.is_empty() {
            return Err(EngineError::BadRequest(failures.join("; ")));
        }
        Ok(())
    }

    /// Hide a workspace from clients and reject new sessions/automation runs
    /// while retaining existing sessions, worktrees, and automation records.
    /// Registering the same path later reopens it.
    pub fn close_workspace(&self, id: &str) -> Result<(), EngineError> {
        self.close_workspace_with(id, |_| {})
    }

    fn close_workspace_with(
        &self,
        id: &str,
        on_lock_attempt: impl FnOnce(bool),
    ) -> Result<(), EngineError> {
        let workspace = self
            .store
            .workspace(id)?
            .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
        // Re-registration may spend time resolving repository identity between
        // reading and reopening an existing row. Serialize close against that
        // whole lifecycle so whichever operation completes last owns the
        // durable visibility and terminal state.
        let registration_lock = self.workspace_registration_lock(Path::new(&workspace.path));
        let _registration =
            Self::acquire_workspace_registration_lock(&registration_lock, on_lock_attempt);
        if self.store.set_workspace_closed(id, true)? {
            for session in self.store.list_sessions(Some(id))? {
                self.terminals.remove_session(&session.id);
            }
            self.store.append_event(
                Scope::Server,
                Event::WorkspaceClosed {
                    workspace_id: id.to_string(),
                },
            )?;
        }
        self.workspace_list_cache.lock().unwrap().remove(id);
        Ok(())
    }

    /// Local branches of the workspace repo, for base-ref selection.
    pub async fn workspace_branches(&self, id: &str) -> Result<BranchList, EngineError> {
        let ws = self
            .store
            .workspace(id)?
            .ok_or_else(|| EngineError::NotFound(format!("workspace {id}")))?;
        let repo = PathBuf::from(&ws.path);
        tokio::task::spawn_blocking(move || -> Result<BranchList> {
            let branches = git::list_branches(&repo)?;
            let head = git::head_ref(&repo)?;
            let default_branch = git::default_branch(&repo);
            Ok(BranchList {
                branches,
                head,
                default_branch,
            })
        })
        .await
        .map_err(|e| EngineError::Internal(anyhow!(e)))?
        .map_err(EngineError::Internal)
    }

    // --- sessions -----------------------------------------------------------

    fn session_create_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.session_create_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }

    fn session_create_request_fingerprint(
        req: &CreateSessionRequest,
    ) -> Result<String, EngineError> {
        serde_json::to_string(&(
            &req.workspace_id,
            &req.title,
            &req.base_ref,
            &req.checkout_ref,
            req.fetch_latest,
        ))
        .map_err(|error| EngineError::Internal(error.into()))
    }

    async fn rollback_failed_session_creation(
        &self,
        worktree: &Path,
        session_id: &str,
        creation: crate::tools::SessionWorktreeCreation,
    ) {
        if let Err(error) = self.executor.evict_worktree(worktree).await {
            // The creation receipt is an independent ownership proof. Always
            // exercise its rollback even when ephemeral cleanup could not be
            // acknowledged; the receipt's compare-and-swap checks fail closed
            // if an owned artifact changed in the meantime.
            tracing::error!(
                session_id,
                %error,
                "failed to evict worktree resources before session creation rollback"
            );
        }

        let cleanup = self
            .executor
            .rollback_session_worktree(crate::tools::SessionWorktreeRollback { creation })
            .await;
        match cleanup {
            Ok(()) => {}
            Err(error) => tracing::error!(
                session_id,
                %error,
                "failed to roll back git artifacts after session creation error"
            ),
        }
    }

    pub async fn create_session(&self, req: CreateSessionRequest) -> Result<Session, EngineError> {
        let create_started = Instant::now();
        let idempotency_key = match req.idempotency_key.as_deref() {
            Some(key)
                if key.is_empty()
                    || key.len() > 128
                    || !key
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)) =>
            {
                return Err(EngineError::BadRequest(
                    "session idempotency key must be 1-128 ASCII letters, digits, '.', '_', or '-'"
                        .into(),
                ));
            }
            Some(key) => Some(key.to_owned()),
            None => None,
        };
        let request_fingerprint = Self::session_create_request_fingerprint(&req)?;
        let create_lock = idempotency_key
            .as_deref()
            .map(|key| self.session_create_lock(key));
        let create_guard = match create_lock {
            Some(lock) => Some(lock.lock_owned().await),
            None => None,
        };
        if let Some(key) = idempotency_key.as_deref()
            && let Some((existing, persisted_fingerprint)) =
                self.store.session_by_create_idempotency_key(key)?
        {
            if persisted_fingerprint != request_fingerprint {
                return Err(EngineError::Conflict(
                    "session idempotency key was already used for a different request".into(),
                ));
            }
            return self.get_session(&existing.id);
        }
        let ws = self
            .store
            .open_workspace(&req.workspace_id)?
            .ok_or_else(|| EngineError::NotFound(format!("workspace {}", req.workspace_id)))?;
        let repo = PathBuf::from(&ws.path);
        let title = req.title.unwrap_or_else(|| "New session".into());
        let session_id = new_id("se");
        let checkpoint_id = new_id("cp");
        let branch = session_branch_name(
            &title,
            &session_id,
            self.title_model.derive_branch_name_from_session_title(),
        );
        let worktree_path = git::worktree_dir(&self.data_dir, &session_id);
        let fetch_latest = req.fetch_latest;
        if self.store.session(&session_id)?.is_some() {
            return Err(EngineError::Conflict(format!(
                "generated session id {session_id} already exists"
            )));
        }
        let creation_request = crate::tools::SessionWorktreeCreate {
            repository: repo,
            worktree: worktree_path.clone(),
            session_id: session_id.clone(),
            checkpoint_id: checkpoint_id.clone(),
            branch: branch.clone(),
            base_ref: req.base_ref,
            checkout_ref: req.checkout_ref,
            fetch_latest: req.fetch_latest,
        };
        // Creation runs in an engine-owned task. If the request future is
        // cancelled after a custom executor has started mutating but before it
        // returns its guard, this task continues; its eventual receipt is then
        // dropped and synchronously rolls the attempt back.
        let executor = self.executor.clone();
        let worktree_started = Instant::now();
        let create_attempt = tokio::spawn(async move {
            SessionWorktreeCreateAttempt {
                creation: executor.create_session_worktree(&creation_request).await,
                idempotency_guard: create_guard,
            }
        })
        .await
        .map_err(|error| EngineError::Internal(anyhow!("session creation task failed: {error}")))?;
        // Keep the task-owned guard in the request after receipt delivery. The
        // receipt is declared later and therefore drops first on cancellation.
        let _create_guard = create_attempt.idempotency_guard;
        let mut creation = create_attempt
            .creation
            .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        tracing::info!(
            %session_id,
            fetch_latest,
            elapsed_ms = worktree_started.elapsed().as_millis(),
            "session startup timing: worktree and initial checkpoint ready"
        );
        let base_ref = creation.base_ref.clone();

        let session = Session {
            id: session_id.clone(),
            workspace_id: ws.id.clone(),
            title,
            branch: branch.clone(),
            worktree_path: worktree_path.to_string_lossy().to_string(),
            base_ref,
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };

        // Checkpoint 0 was created atomically with the executor-owned worktree
        // receipt, so cancellation between those two mutations cannot leak
        // either artifact.
        let commit = creation.checkpoint_commit.clone();
        let checkpoint = CheckpointRow {
            id: checkpoint_id.clone(),
            session_id: session_id.clone(),
            thread_id: None,
            turn: 0,
            seq: 0,
            commit_hash: commit.clone(),
        };

        let inserted = self.store.insert_session_with_lifecycle(
            &session,
            &checkpoint,
            idempotency_key
                .as_deref()
                .map(|key| (key, request_fingerprint.as_str())),
            vec![
                (
                    Scope::Server,
                    Event::SessionCreated {
                        session_id: session_id.clone(),
                        workspace_id: ws.id.clone(),
                    },
                ),
                (
                    Scope::Session(session_id.clone()),
                    Event::WorktreeCreated {
                        path: session.worktree_path.clone(),
                        branch: branch.clone(),
                    },
                ),
                (
                    Scope::Session(session_id.clone()),
                    Event::CheckpointCreated {
                        checkpoint_id,
                        thread_id: String::new(),
                        turn: 0,
                        commit: commit.clone(),
                    },
                ),
            ],
        );
        match inserted {
            Ok(_) => {
                // Intentionally synchronous and immediately after commit.
                creation.mark_durable();
            }
            Err(error) => {
                if let Some(key) = idempotency_key.as_deref() {
                    match self.store.session_by_create_idempotency_key(key) {
                        Ok(Some((_existing, persisted_fingerprint)))
                            if persisted_fingerprint != request_fingerprint =>
                        {
                            self.rollback_failed_session_creation(
                                &worktree_path,
                                &session_id,
                                creation,
                            )
                            .await;
                            return Err(EngineError::Conflict(
                                "session idempotency key was already used for a different request"
                                    .into(),
                            ));
                        }
                        Ok(Some((existing, _))) if existing.id != session_id => {
                            self.rollback_failed_session_creation(
                                &worktree_path,
                                &session_id,
                                creation,
                            )
                            .await;
                            return Ok(existing);
                        }
                        Ok(Some(_)) => {
                            tracing::warn!(
                                session_id,
                                %error,
                                "idempotent session insert reply failed after durable commit"
                            );
                            creation.mark_durable();
                        }
                        Ok(None) => {
                            self.rollback_failed_session_creation(
                                &worktree_path,
                                &session_id,
                                creation,
                            )
                            .await;
                            return Err(error.into());
                        }
                        Err(inspect_error) => {
                            creation.preserve_for_recovery();
                            return Err(EngineError::Internal(error.context(format!(
                                "session insert outcome is ambiguous and its ownership marker was retained: {inspect_error:#}"
                            ))));
                        }
                    }
                } else {
                    match self.store.session(&session_id) {
                        Ok(Some(_)) => {
                            // The writer may have committed and then lost its reply.
                            // The relational mutation and lifecycle events share that
                            // transaction, so an existing row proves durability.
                            tracing::warn!(session_id, %error, "session insert reply failed after durable commit");
                            creation.mark_durable();
                        }
                        Ok(None) => {
                            self.rollback_failed_session_creation(
                                &worktree_path,
                                &session_id,
                                creation,
                            )
                            .await;
                            return Err(error.into());
                        }
                        Err(inspect_error) => {
                            creation.preserve_for_recovery();
                            return Err(EngineError::Internal(error.context(format!(
                                "session insert outcome is ambiguous and its ownership marker was retained: {inspect_error:#}"
                            ))));
                        }
                    }
                }
            }
        }
        match self.executor.finalize_session_worktree(creation).await {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(session_id, %error, "failed to remove session creation marker");
            }
        }
        if self.index_hooks {
            crate::tools::warm_index_in_background(worktree_path);
        }
        tracing::info!(
            %session_id,
            elapsed_ms = create_started.elapsed().as_millis(),
            "session startup timing: session created"
        );
        Ok(session)
    }

    pub fn list_sessions(&self, workspace_id: Option<&str>) -> Result<Vec<Session>, EngineError> {
        let mut sessions = self.store.list_sessions(workspace_id)?;
        let active = self.active_threads.lock().unwrap();
        for session in &mut sessions {
            session.active = active.values().any(|s| *s == session.id);
        }
        Ok(sessions)
    }

    pub fn session_summaries_snapshot(
        &self,
    ) -> Result<trouve_protocol::SessionSummariesSnapshot, EngineError> {
        Ok(self.store.session_summaries_snapshot()?)
    }

    pub fn get_session(&self, id: &str) -> Result<Session, EngineError> {
        let mut session = self
            .store
            .session(id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {id}")))?;
        session.active = {
            let active = self.active_threads.lock().unwrap();
            active.values().any(|s| *s == session.id)
        };
        Ok(session)
    }

    /// Rename and/or (un)archive a session.
    pub fn update_session(
        &self,
        id: &str,
        req: &UpdateSessionRequest,
    ) -> Result<Session, EngineError> {
        self.get_session(id)?;
        if let Some(title) = req.title.as_deref()
            && title.trim().is_empty()
        {
            return Err(EngineError::BadRequest("title cannot be empty".into()));
        }
        if req.expected_title.is_some() && req.title.is_none() {
            return Err(EngineError::BadRequest(
                "expected_title requires a title update".into(),
            ));
        }
        let updated = {
            // Serialize mutation against delete's marker. Besides preserving
            // the session row, this prevents an unarchive from reopening the
            // terminal manager while deletion is tearing the session down.
            let _activity_publication = self.session_activity_publication.lock().unwrap();
            let active_threads = self.active_threads.lock().unwrap();
            let deleting = self.deleting_sessions.lock().unwrap();
            if deleting.contains(id) {
                return Err(EngineError::Conflict(format!(
                    "session {id} is being deleted"
                )));
            }
            let mut session = self
                .store
                .session(id)?
                .ok_or_else(|| EngineError::NotFound(format!("session {id}")))?;
            if let Some(expected_title) = req.expected_title.as_deref()
                && session.title != expected_title
            {
                return Err(EngineError::Conflict(format!(
                    "session {id} title changed before the generated title was ready"
                )));
            }
            let newly_archived = req.archived == Some(true) && !session.archived;
            let newly_unarchived = req.archived == Some(false) && session.archived;
            self.store.update_session_with_event(
                id,
                req.title.as_deref(),
                req.archived,
                req.expected_title.as_deref(),
                Event::SessionUpdated {
                    session_id: id.to_string(),
                    workspace_id: session.workspace_id.clone(),
                },
            )?;
            if let Some(title) = req.title.as_ref() {
                session.title = title.clone();
            }
            if let Some(archived) = req.archived {
                session.archived = archived;
            }
            session.active = active_threads.values().any(|owner| owner == id);
            if newly_archived {
                self.terminals.remove_session(id);
            } else if newly_unarchived {
                self.terminals.reopen_session(id);
            }
            if self.index_hooks
                && newly_archived
                && let Some(ws) = self.store.workspace(&session.workspace_id)?
            {
                crate::tools::gc_index_store_in_background(PathBuf::from(&ws.path));
            }
            session
        };
        Ok(updated)
    }

    pub async fn delete_session(&self, id: &str) -> Result<(), EngineError> {
        self.get_session(id)?;
        {
            // Lock ordering is always activity publication -> active_threads
            // -> deleting_sessions; dispatch_queue uses the same order. This
            // also waits for a pending idle event before admitting deletion.
            // Once the marker is set, an idle session cannot acquire a new
            // dispatcher while deletion is in progress.
            let _activity_publication = self.session_activity_publication.lock().unwrap();
            let active = self.active_threads.lock().unwrap();
            if active.values().any(|session_id| session_id == id) {
                return Err(EngineError::Conflict(format!(
                    "session {id} has an active turn"
                )));
            }
            let mut deleting = self.deleting_sessions.lock().unwrap();
            if !deleting.insert(id.to_string()) {
                return Err(EngineError::Conflict(format!(
                    "session {id} is already being deleted"
                )));
            }
        }
        // The marker must clear even if this future is aborted while waiting
        // for the lifecycle lease or post-commit cleanup.
        let _deleting_marker = DeletingSessionMarker {
            sessions: &self.deleting_sessions,
            session_id: id.to_string(),
        };

        // The marker rejects new lock-free mutations; the exclusive
        // lifecycle lease waits out settings changes, restores, and any
        // already-admitted turn before relational or filesystem teardown.
        let lifecycle = self.session_lock(id);
        let _lifecycle_guard = lifecycle.write().await;

        async {
            let session = self
                .store
                .session(id)?
                .ok_or_else(|| EngineError::NotFound(format!("session {id}")))?;
            let bridge_thread_ids = self
                .store
                .list_threads(id)?
                .into_iter()
                .map(|thread| thread.id)
                .collect::<Vec<_>>();
            let ws = self
                .store
                .workspace(&session.workspace_id)?
                .ok_or_else(|| {
                    EngineError::NotFound(format!("workspace {}", session.workspace_id))
                })?;
            // Capture cleanup paths while the relational rows still exist,
            // then commit the database deletion before any irreversible
            // filesystem work. A database error must leave the session and
            // its worktree consistently intact.
            let attachment_paths = self.store.session_attachment_paths(id)?;
            let cleanup_job = ArtifactCleanupJob::deleted_session(
                id.to_string(),
                session.worktree_path.clone(),
                ws.path.clone(),
                attachment_paths,
            );
            self.store.delete_session_with_event(
                id,
                cleanup_job.clone(),
                Event::SessionDeleted {
                    session_id: id.to_string(),
                    workspace_id: session.workspace_id.clone(),
                },
            )?;
            // Only kill ephemeral terminals after the durable deletion has
            // committed. A database error therefore leaves live PTYs intact.
            self.terminals.remove_session(id);
            for thread_id in bridge_thread_ids {
                self.revoke_bridge_tickets(&thread_id);
                self.bridged_tool_owners.clear_root(&thread_id);
            }
            // The cleanup job was committed with the tombstone. Scheduling is
            // best effort here; startup retries any intent left behind by an
            // aborted request or process crash.
            self.schedule_artifact_cleanup(cleanup_job);
            Ok(())
        }
        .await
    }

    // --- threads ------------------------------------------------------------

    pub fn create_thread(&self, req: CreateThreadRequest) -> Result<Thread, EngineError> {
        let session = self.get_session(&req.session_id)?;
        self.create_thread_for_session(session, req)
    }

    /// Create a thread from already-loaded session metadata. Provider-native
    /// collaborator creation uses this while holding the active-thread
    /// registry lock, so it must not call `get_session` (which reads that same
    /// registry to project `Session.active`).
    fn create_thread_for_session(
        &self,
        session: Session,
        req: CreateThreadRequest,
    ) -> Result<Thread, EngineError> {
        self.create_thread_for_session_with_parent(session, req, None)
    }

    fn create_spawned_thread_for_session(
        &self,
        session: Session,
        req: CreateThreadRequest,
        parent_thread_id: &str,
        kind: &str,
    ) -> Result<Thread, EngineError> {
        self.create_thread_for_session_with_parent(session, req, Some((parent_thread_id, kind)))
    }

    fn create_thread_for_session_with_parent(
        &self,
        session: Session,
        req: CreateThreadRequest,
        spawn: Option<(&str, &str)>,
    ) -> Result<Thread, EngineError> {
        debug_assert_eq!(session.id, req.session_id);
        let ws = self.store.workspace(&session.workspace_id)?.unwrap();
        let all_modes = self.resolve_personas(Some(Path::new(&ws.path)))?;
        let mode_id = req.mode.unwrap_or_else(|| "code".into());
        let mode = personas::find_persona(&all_modes, &mode_id)
            .ok_or_else(|| EngineError::BadRequest(format!("unknown persona: {mode_id}")))?;
        // Provider availability is validated when a message is sent, not
        // here: a thread must be creatable before any provider is configured.
        // Model precedence: explicit request > the mode's default model >
        // the global default.
        let global_defaults = self.global_defaults.read().unwrap().clone();
        let model = req
            .model
            .or_else(|| mode.default_model.clone())
            .unwrap_or_else(|| global_defaults.model.clone());
        let mut model_options = req.model_options;
        // `thinking_level` is the canonical inherited key. Before a turn it
        // is resolved to the selected model's advertised key
        // (reasoning_effort, effort, ...), or removed for models that do not
        // expose thinking levels.
        inherit_thinking_option(
            &mut model_options,
            mode.default_thinking_level.as_deref(),
            global_defaults.thinking_level.as_deref(),
        );
        let thread = Thread {
            id: new_id("th"),
            session_id: session.id.clone(),
            parent_thread_id: spawn.map(|(parent, _)| parent.to_string()),
            title: req
                .title
                .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
                .map(|title| title.chars().take(96).collect::<String>())
                .filter(|title| !title.is_empty()),
            mode: mode.id.clone(),
            model,
            model_options: model_options.clone(),
            // Permission precedence mirrors the model's: explicit request >
            // the mode's default > the global default.
            permission_mode: req
                .permission_mode
                .or(mode.default_permission_mode)
                .unwrap_or(global_defaults.permission_mode),
            created_at: chrono::Utc::now(),
            spawned: spawn.is_some(),
            todos: Vec::new(),
        };
        // Hold the deletion marker lock through relational insertion and the
        // source event. A delete that has not marked the session yet waits;
        // once marked, no new thread can materialize behind its snapshot.
        let deleting = self.deleting_sessions.lock().unwrap();
        if deleting.contains(&session.id) {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                session.id
            )));
        }
        let live_session = self
            .store
            .session(&session.id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {}", session.id)))?;
        self.store.insert_thread_with_event(
            &thread,
            &model_options,
            spawn,
            Event::ThreadCreated {
                thread_id: thread.id.clone(),
                session_id: live_session.id,
            },
        )?;
        drop(deleting);
        Ok(thread)
    }

    pub fn get_thread(&self, id: &str) -> Result<Thread, EngineError> {
        self.store
            .thread(id)?
            .ok_or_else(|| EngineError::NotFound(format!("thread {id}")))
    }

    /// Spawn parentage controls hierarchy, while the selected data-driven
    /// mode controls whether a child is an audit transcript or an interactive
    /// conversation. Unknown/missing modes fail closed.
    fn subagent_is_read_only(&self, thread: &Thread) -> Result<bool, EngineError> {
        if !thread.spawned {
            return Ok(false);
        }
        let session = self.get_session(&thread.session_id)?;
        let workspace = self
            .store
            .workspace(&session.workspace_id)?
            .ok_or_else(|| EngineError::NotFound("workspace".into()))?;
        let modes = self.resolve_personas(Some(Path::new(&workspace.path)))?;
        Ok(personas::find_persona(&modes, &thread.mode)
            .map(|mode| mode.read_only)
            .unwrap_or(true))
    }

    fn backend_collaborator_mode(
        &self,
        session: &Session,
        inherited_thread: &Thread,
        access: BackendCollaboratorAccess,
    ) -> Result<String, EngineError> {
        let workspace = self
            .store
            .workspace(&session.workspace_id)?
            .ok_or_else(|| EngineError::NotFound("workspace".into()))?;
        let modes = self.resolve_personas(Some(Path::new(&workspace.path)))?;
        let inherited = personas::find_persona(&modes, &inherited_thread.mode);
        // Provider metadata may reduce a collaborator's authority, but it is
        // never an authorization source that can widen the parent's mode.
        // Unknown parent modes fail closed just like turn execution does.
        let inherited_read_only = inherited.is_none_or(|mode| mode.read_only);
        let access = match access {
            BackendCollaboratorAccess::Interactive if inherited_read_only => {
                tracing::warn!(
                    parent_thread_id = %inherited_thread.id,
                    parent_mode = %inherited_thread.mode,
                    "provider requested writable collaborator under a read-only parent; clamping access"
                );
                BackendCollaboratorAccess::ReadOnly
            }
            access => access,
        };
        if access == BackendCollaboratorAccess::Inherit {
            return Ok(inherited_thread.mode.clone());
        }
        let read_only = access == BackendCollaboratorAccess::ReadOnly;
        let inherited_matches = inherited.is_some_and(|mode| mode.read_only == read_only);
        if inherited_matches {
            return Ok(inherited_thread.mode.clone());
        }
        let preferred = match access {
            BackendCollaboratorAccess::ReadOnly => ["plan", "review"].as_slice(),
            BackendCollaboratorAccess::Interactive => ["code"].as_slice(),
            BackendCollaboratorAccess::Inherit => unreachable!(),
        };
        preferred
            .iter()
            .find_map(|id| {
                personas::find_persona(&modes, id).filter(|mode| mode.read_only == read_only)
            })
            .or_else(|| modes.iter().find(|mode| mode.read_only == read_only))
            .map(|mode| mode.id.clone())
            .ok_or_else(|| {
                EngineError::BadRequest(format!(
                    "no {} mode is configured for this subagent",
                    if access == BackendCollaboratorAccess::ReadOnly {
                        "read-only"
                    } else {
                        "interactive"
                    }
                ))
            })
    }

    pub fn thread_view_snapshot(
        &self,
        id: &str,
        before: Option<u64>,
        limit: usize,
        turn_aligned: bool,
    ) -> Result<(u64, trouve_protocol::ThreadViewSnapshot), EngineError> {
        self.get_thread(id)?;
        Ok(self
            .store
            .thread_view_snapshot(id, before, limit, turn_aligned)?)
    }

    pub fn thread_tool_details(
        &self,
        thread_id: &str,
        call_id: &str,
    ) -> Result<trouve_protocol::ThreadToolDetails, EngineError> {
        self.get_thread(thread_id)?;
        self.store
            .thread_tool_details(thread_id, call_id)?
            .ok_or_else(|| {
                EngineError::NotFound(format!("tool call {call_id} in thread {thread_id}"))
            })
    }

    pub fn list_threads(&self, session_id: &str) -> Result<Vec<Thread>, EngineError> {
        Ok(self.store.list_threads(session_id)?)
    }

    pub fn list_thread_subagents(&self, thread_id: &str) -> Result<Vec<Thread>, EngineError> {
        self.get_thread(thread_id)?;
        let mut children = self
            .store
            .spawned_children(thread_id)?
            .into_iter()
            .map(|child_id| self.store.thread(&child_id))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        children.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(children)
    }

    /// List every durable provider-native or trouve-owned descendant below a
    /// thread. The direct-child endpoint remains the default contract; the
    /// Details panel opts into this recursive projection so a nested worker
    /// cannot remain active while disappearing from the parent's overview.
    pub fn list_thread_descendants(&self, thread_id: &str) -> Result<Vec<Thread>, EngineError> {
        self.get_thread(thread_id)?;
        Ok(self.store.spawned_descendants(thread_id)?)
    }

    /// Root thread and number of spawn edges above `thread_id`. Parentage is
    /// persisted across sessions, so this works for mixed spawn_thread /
    /// spawn_session trees. Corrupt cycles fail closed instead of permitting
    /// an unbounded delegation loop.
    fn subagent_root_and_depth(&self, thread_id: &str) -> Result<(String, usize), EngineError> {
        self.get_thread(thread_id)?;
        let mut current = thread_id.to_string();
        let mut depth = 0usize;
        let mut seen = HashSet::from([current.clone()]);
        while let Some(parent) = self.store.spawn_parent(&current)? {
            if !seen.insert(parent.clone()) {
                return Err(EngineError::Internal(anyhow!(
                    "cycle in spawned-thread parentage at {parent}"
                )));
            }
            current = parent;
            depth += 1;
        }
        Ok((current, depth))
    }

    fn thread_can_spawn_subagents(&self, thread_id: &str) -> Result<bool, EngineError> {
        Ok(self.subagent_root_and_depth(thread_id)?.1 < MAX_SUBAGENT_DEPTH)
    }

    fn subagent_tree_lock(&self, root_thread_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.subagent_tree_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(root_thread_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(root_thread_id.to_string(), Arc::downgrade(&lock));
        lock
    }

    pub fn list_thread_statuses(
        &self,
        session_id: &str,
    ) -> Result<Vec<trouve_protocol::ThreadStatus>, EngineError> {
        Ok(self.store.list_thread_statuses(session_id)?)
    }

    /// Change thread settings (mode/model/options) between turns. Conflicts
    /// only while a turn is running on this thread.
    pub fn update_thread(
        &self,
        id: &str,
        req: &UpdateThreadRequest,
    ) -> Result<Thread, EngineError> {
        let thread = self.get_thread(id)?;
        if self.subagent_is_read_only(&thread)? {
            return Err(EngineError::Conflict(
                "this subagent uses a read-only exploration, audit, or review mode".into(),
            ));
        }
        let session = self.get_session(&thread.session_id)?;

        // Serialize this check with prompt dispatch so a turn cannot start on
        // this thread until its settings update has been persisted. Sibling
        // threads have independent settings and do not block one another.
        let active_threads = self.active_threads.lock().unwrap();
        if active_threads.contains_key(id) {
            return Err(EngineError::Conflict(
                "cannot change thread settings while this thread is running a turn".into(),
            ));
        }
        let deleting = self.deleting_sessions.lock().unwrap();
        if deleting.contains(&session.id) {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                session.id
            )));
        }
        let thread = self
            .store
            .thread(id)?
            .ok_or_else(|| EngineError::NotFound(format!("thread {id}")))?;
        let session = self
            .store
            .session(&thread.session_id)?
            .ok_or_else(|| EngineError::NotFound(format!("session {}", thread.session_id)))?;

        if let Some(mode_id) = req.mode.as_deref() {
            let ws = self.store.workspace(&session.workspace_id)?.unwrap();
            let all_modes = self.resolve_personas(Some(Path::new(&ws.path)))?;
            personas::find_persona(&all_modes, mode_id)
                .ok_or_else(|| EngineError::BadRequest(format!("unknown persona: {mode_id}")))?;
        }
        if let Some(model) = req.model.as_deref()
            && !model.contains('/')
        {
            return Err(EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            )));
        }
        self.store.update_thread_with_event(
            id,
            req.mode.as_deref(),
            req.model.as_deref(),
            req.model_options.as_ref(),
            req.permission_mode,
            Event::ThreadUpdated {
                thread_id: id.to_string(),
                session_id: session.id,
            },
        )?;
        drop(active_threads);
        drop(deleting);
        self.get_thread(id)
    }

    fn resolve_provider(&self, model: &str) -> Result<(Arc<dyn Provider>, String), EngineError> {
        let (provider_id, model_name) = model.split_once('/').ok_or_else(|| {
            EngineError::BadRequest(format!(
                "model must be provider-qualified (e.g. openai/gpt-4.1-mini): {model}"
            ))
        })?;
        let provider = self
            .providers
            .read()
            .unwrap()
            .get(provider_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::BadRequest(format!(
                    "provider {provider_id} is not configured (configured: {})",
                    self.provider_ids().join(", ")
                ))
            })?;
        Ok((provider, model_name.to_string()))
    }

    pub(crate) async fn resolve_model_info(
        &self,
        model: &str,
    ) -> Result<trouve_protocol::ModelInfo, EngineError> {
        if let Some((_, backend, _)) = self.backend_for(model) {
            let models =
                tokio::time::timeout(MODEL_CATALOG_VALIDATION_TIMEOUT, backend.list_models())
                    .await
                    .map_err(|_| {
                        EngineError::BadRequest(format!(
                            "timed out loading model metadata for {model}"
                        ))
                    })?;
            return models
                .into_iter()
                .find(|candidate| candidate.id == model)
                .ok_or_else(|| {
                    EngineError::BadRequest(format!(
                        "model {model} is not available from its configured provider"
                    ))
                });
        }
        let (provider, _) = self.resolve_provider(model)?;
        let live = tokio::time::timeout(MODEL_CATALOG_VALIDATION_TIMEOUT, provider.list_models())
            .await
            .map_err(|_| {
                EngineError::BadRequest(format!("timed out loading model metadata for {model}"))
            })?;
        let known = provider.models();
        live.into_iter()
            .chain(known)
            .find(|candidate| candidate.id == model)
            .ok_or_else(|| {
                EngineError::BadRequest(format!(
                    "model {model} is not available from its configured provider"
                ))
            })
    }

    // --- approvals ------------------------------------------------------------

    pub fn resolve_approval(
        &self,
        thread_id: &str,
        call_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), EngineError> {
        match self.approvals.resolve(thread_id, call_id, decision) {
            ApprovalResolution::Resolved => Ok(()),
            ApprovalResolution::NotFound => {
                Err(EngineError::NotFound(format!("pending approval {call_id}")))
            }
        }
    }

    // --- questions --------------------------------------------------------------

    /// Answer (or skip, `answers: None`) a pending `question.requested`.
    pub fn resolve_question(
        &self,
        thread_id: &str,
        request_id: &str,
        answers: Option<Vec<trouve_protocol::QuestionAnswer>>,
    ) -> Result<(), EngineError> {
        match self.questions.resolve(thread_id, request_id, answers) {
            QuestionResolution::Resolved => Ok(()),
            QuestionResolution::NotFound => Err(EngineError::NotFound(format!(
                "pending question {request_id}"
            ))),
        }
    }

    /// Pose questions to the user and block until they answer or skip.
    /// Emits `question.requested` / `question.resolved` around the wait.
    async fn ask_user_questions(
        &self,
        thread_id: &str,
        turn: u64,
        request_id: &str,
        title: Option<String>,
        questions: Vec<trouve_protocol::Question>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Option<Vec<trouve_protocol::QuestionAnswer>>> {
        let scope = Scope::Thread(thread_id.to_string());
        let rx = self
            .questions
            .request(thread_id, request_id)
            .with_context(|| {
                format!("duplicate pending question {request_id} in thread {thread_id}")
            })?;
        let mut cleanup = PendingQuestionCleanup {
            questions: self.questions.clone(),
            store: self.store.clone(),
            scope: scope.clone(),
            thread_id: thread_id.to_string(),
            request_id: request_id.to_string(),
            armed: true,
            requested_persisted: false,
        };
        self.store.append_event(
            scope.clone(),
            Event::QuestionRequested {
                turn,
                request_id: request_id.to_string(),
                title,
                questions,
            },
        )?;
        cleanup.requested_persisted = true;
        let answers = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Remove the pending sender so a late HTTP answer cannot
                // target a turn that has already entered cleanup.
                let _ = self.questions.resolve(thread_id, request_id, None);
                None
            }
            answers = rx => answers.unwrap_or(None),
        };
        let answers = if cancel.is_cancelled() { None } else { answers };
        self.store.append_event(
            scope,
            Event::QuestionResolved {
                request_id: request_id.to_string(),
                answers: answers.clone(),
            },
        )?;
        cleanup.armed = false;
        Ok(answers)
    }

    // --- undo/redo --------------------------------------------------------------

    pub async fn undo(self: &Arc<Self>, session_id: &str) -> Result<(), EngineError> {
        self.restore_checkpoint(session_id, RestoreDirection::Undo)
            .await
    }

    pub async fn redo(self: &Arc<Self>, session_id: &str) -> Result<(), EngineError> {
        self.restore_checkpoint(session_id, RestoreDirection::Redo)
            .await
    }

    /// Restore the worktree to one exact turn checkpoint rather than taking
    /// a relative step through the session's undo stack.
    pub async fn restore_checkpoint_by_id(&self, checkpoint_id: &str) -> Result<(), EngineError> {
        let cp = self
            .store
            .checkpoint(checkpoint_id)?
            .ok_or_else(|| EngineError::NotFound(format!("checkpoint {checkpoint_id}")))?;
        let session = self.get_session(&cp.session_id)?;
        let lock = self.session_lock(&session.id);
        let _guard = lock.write().await;
        if self.deleting_sessions.lock().unwrap().contains(&session.id) {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                session.id
            )));
        }

        let latest = self
            .store
            .latest_checkpoint_seq(&session.id)?
            .ok_or_else(|| EngineError::BadRequest("session has no checkpoints".into()))?;
        let cp = self
            .store
            .checkpoint(checkpoint_id)?
            .filter(|current| current.session_id == session.id)
            .ok_or_else(|| EngineError::NotFound(format!("checkpoint {checkpoint_id}")))?;
        if cp.seq < 0 || cp.seq > latest {
            return Err(EngineError::NotFound(format!(
                "checkpoint {checkpoint_id} is no longer available"
            )));
        }
        self.restore_checkpoint_row(&session, cp, latest, RestoreDirection::Exact)
            .await
    }

    /// Create a new session whose worktree starts at one exact turn
    /// checkpoint. The initial thread inherits the source thread's current
    /// mode, model, model options, and permission policy.
    pub async fn fork_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> Result<ForkCheckpointResponse, EngineError> {
        let checkpoint = self
            .store
            .checkpoint(checkpoint_id)?
            .ok_or_else(|| EngineError::NotFound(format!("checkpoint {checkpoint_id}")))?;
        let source_session = self.get_session(&checkpoint.session_id)?;
        let source_thread_id = checkpoint.thread_id.as_deref().ok_or_else(|| {
            EngineError::BadRequest("the session-start checkpoint has no source thread".into())
        })?;
        let source_thread = self.get_thread(source_thread_id)?;
        if source_thread.session_id != source_session.id {
            return Err(EngineError::Internal(anyhow!(
                "checkpoint {checkpoint_id} references a thread outside its session"
            )));
        }

        let session = self
            .create_session(CreateSessionRequest {
                workspace_id: source_session.workspace_id,
                idempotency_key: None,
                title: Some(format!(
                    "{} (fork after turn {})",
                    source_session.title, checkpoint.turn
                )),
                base_ref: Some(source_session.base_ref),
                checkout_ref: Some(checkpoint.commit_hash),
                fetch_latest: false,
            })
            .await?;
        let thread = match self.create_thread(CreateThreadRequest {
            session_id: session.id.clone(),
            title: Some(session.title.clone()),
            mode: Some(source_thread.mode),
            model: Some(source_thread.model),
            model_options: source_thread.model_options,
            permission_mode: Some(source_thread.permission_mode),
        }) {
            Ok(thread) => thread,
            Err(error) => {
                if let Err(cleanup_error) = self.delete_session(&session.id).await {
                    tracing::warn!(
                        session_id = %session.id,
                        "failed to clean up checkpoint fork after thread creation failed: {cleanup_error}"
                    );
                }
                return Err(error);
            }
        };

        Ok(ForkCheckpointResponse { session, thread })
    }

    async fn restore_checkpoint(
        &self,
        session_id: &str,
        direction: RestoreDirection,
    ) -> Result<(), EngineError> {
        let session = self.get_session(session_id)?;
        let lock = self.session_lock(session_id);
        let _guard = lock.write().await;
        if self.deleting_sessions.lock().unwrap().contains(session_id) {
            return Err(EngineError::Conflict(format!(
                "session {session_id} is being deleted"
            )));
        }

        let latest = self
            .store
            .latest_checkpoint_seq(session_id)?
            .ok_or_else(|| EngineError::BadRequest("session has no checkpoints".into()))?;
        let current = self.store.undo_pos(session_id)?.unwrap_or(latest);
        let target = match direction {
            RestoreDirection::Undo => current - 1,
            RestoreDirection::Redo => current + 1,
            RestoreDirection::Exact => {
                return Err(EngineError::BadRequest(
                    "an exact restore requires a checkpoint id".into(),
                ));
            }
        };
        if target < 0 || target > latest {
            return Err(EngineError::BadRequest(format!(
                "nothing to {}",
                match direction {
                    RestoreDirection::Undo => "undo",
                    RestoreDirection::Redo => "redo",
                    RestoreDirection::Exact => "restore",
                }
            )));
        }
        let cp = self
            .store
            .checkpoint_at(session_id, target)?
            .ok_or_else(|| EngineError::NotFound(format!("checkpoint seq {target}")))?;
        self.restore_checkpoint_row(&session, cp, latest, direction)
            .await?;
        Ok(())
    }

    async fn restore_checkpoint_row(
        &self,
        session: &Session,
        checkpoint: CheckpointRow,
        latest: i64,
        direction: RestoreDirection,
    ) -> Result<(), EngineError> {
        let wt = PathBuf::from(&session.worktree_path);
        let commit = checkpoint.commit_hash.clone();
        tokio::task::spawn_blocking(move || git::restore(&wt, &commit))
            .await
            .map_err(|e| EngineError::Internal(anyhow!(e)))?
            .map_err(EngineError::Internal)?;
        self.store.set_undo_pos(
            &session.id,
            if checkpoint.seq == latest {
                None
            } else {
                Some(checkpoint.seq)
            },
        )?;
        self.store.append_event(
            Scope::Session(session.id.clone()),
            Event::CheckpointRestored {
                checkpoint_id: checkpoint.id,
                direction,
            },
        )?;
        Ok(())
    }

    // --- turns ---------------------------------------------------------------

    /// Accept a user message. If the thread is idle it runs immediately;
    /// otherwise it joins the thread's persistent prompt queue and runs when
    /// its turn comes. Progress is visible on the thread's event stream.
    /// Attachment uploads are decoded and stored immediately, so queued
    /// prompts reference durable files rather than request payloads.
    pub fn send_message(
        self: &Arc<Self>,
        thread_id: &str,
        content: String,
        uploads: Vec<trouve_protocol::AttachmentUpload>,
    ) -> Result<TurnAccepted, EngineError> {
        self.send_message_with_tools(thread_id, content, uploads, true, false)
    }

    pub(crate) fn send_message_without_tools(
        self: &Arc<Self>,
        thread_id: &str,
        content: String,
    ) -> Result<TurnAccepted, EngineError> {
        self.send_message_with_tools(thread_id, content, Vec::new(), false, false)
    }

    fn turn_shell_events(
        &self,
        thread: &Thread,
        turn: u64,
        prompt: &trouve_protocol::QueuedPrompt,
        tools_enabled: bool,
    ) -> Result<Vec<Event>, EngineError> {
        let mut model_options = self.store.thread_model_options(&thread.id)?;
        let (selected_model, supports_steering) =
            if let Some((_backend_id, backend, _model_name)) = self.backend_for(&thread.model) {
                (
                    backend
                        .models()
                        .into_iter()
                        .find(|model| model.id == thread.model),
                    tools_enabled && backend.supports_steering(),
                )
            } else {
                (
                    self.resolve_provider(&thread.model)
                        .ok()
                        .and_then(|(provider, _)| {
                            provider
                                .models()
                                .into_iter()
                                .find(|model| model.id == thread.model)
                        }),
                    false,
                )
            };
        if selected_model.is_some() {
            normalize_thinking_option(&mut model_options, selected_model.as_ref());
        }
        let thinking_level = resolved_thinking_level(&model_options, selected_model.as_ref());
        Ok(vec![
            Event::TurnStarted {
                turn,
                mode: thread.mode.clone(),
                model: thread.model.clone(),
                thinking_level,
                supports_steering,
            },
            Event::UserMessage {
                turn,
                content: prompt.content.clone(),
                attachments: prompt.attachments.clone(),
                background: prompt.background,
            },
        ])
    }

    /// Append user guidance to the exact backend turn currently running on a
    /// thread. The backend loop owns acceptance and durable ordering so a
    /// steer cannot jump ahead of output already received from the vendor.
    pub async fn steer_turn(
        &self,
        thread_id: &str,
        content: String,
        uploads: Vec<trouve_protocol::AttachmentUpload>,
    ) -> Result<trouve_protocol::SteerAccepted, EngineError> {
        let thread = self.get_thread(thread_id)?;
        if self.subagent_is_read_only(&thread)? {
            return Err(EngineError::Conflict(
                "this subagent uses a read-only exploration, audit, or review mode".into(),
            ));
        }
        if content.trim().is_empty() && uploads.is_empty() {
            return Err(EngineError::BadRequest(
                "a steering message needs text or an attachment".into(),
            ));
        }
        // The registration belongs to the exact running turn and is the
        // capability authority. Do not re-read the thread's selected model:
        // an API client can change that selection while the previous model's
        // turn is still unwinding, but steering must still target that turn.
        let active = self
            .turn_steerers
            .lock()
            .unwrap()
            .get(thread_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::Conflict(format!("thread {thread_id} has no steerable turn ready"))
            })?;
        let (prepared, attachment_cleanup) = self.prepare_attachments(uploads)?;
        let attachments = prepared
            .iter()
            .map(|(attachment, _)| attachment.clone())
            .collect::<Vec<_>>();
        let attachment_rows = prepared
            .iter()
            .map(|(attachment, path)| (attachment.clone(), path.to_string_lossy().into_owned()))
            .collect();
        let (response, accepted) = tokio::sync::oneshot::channel();
        if active
            .sender
            .send(SteerTurnCommand {
                content,
                attachments,
                attachment_rows,
                attachment_cleanup,
                response,
            })
            .await
            .is_err()
        {
            return Err(EngineError::Conflict(format!(
                "turn {} finished before it could be steered",
                active.turn
            )));
        }
        match accepted.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(EngineError::Conflict(error));
            }
            Err(_) => {
                return Err(EngineError::Conflict(format!(
                    "turn {} finished before steering was acknowledged",
                    active.turn
                )));
            }
        }
        Ok(trouve_protocol::SteerAccepted {
            thread_id: thread_id.to_string(),
            turn: active.turn,
        })
    }

    /// Whether the current turn has accepted attachment-bearing steering and
    /// is waiting for the session mutation lane. Exposed for server-level
    /// lifecycle tests and diagnostics; callers must treat it as transient.
    #[doc(hidden)]
    pub async fn wait_for_steer_mutation_lane(&self, thread_id: &str) -> bool {
        let Some(mut state) = self
            .turn_steerers
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|active| active.mutation_lane_state.subscribe())
        else {
            return false;
        };
        loop {
            match *state.borrow_and_update() {
                SteerMutationLaneState::Waiting => return true,
                SteerMutationLaneState::Ended => return false,
                SteerMutationLaneState::Idle => {}
            }
            if state.changed().await.is_err() {
                return false;
            }
        }
    }

    fn send_message_with_tools(
        self: &Arc<Self>,
        thread_id: &str,
        content: String,
        uploads: Vec<trouve_protocol::AttachmentUpload>,
        tools_enabled: bool,
        allow_spawned: bool,
    ) -> Result<TurnAccepted, EngineError> {
        self.send_message_inner(
            thread_id,
            content,
            uploads,
            tools_enabled,
            allow_spawned,
            false,
        )
    }

    /// Full prompt-submission path. `background` marks a server-dispatched
    /// attach turn for vendor-autonomous activity; it is trusted dispatch
    /// metadata carried on the queued prompt, never inferred from content.
    #[allow(clippy::too_many_arguments)]
    fn send_message_inner(
        self: &Arc<Self>,
        thread_id: &str,
        content: String,
        uploads: Vec<trouve_protocol::AttachmentUpload>,
        tools_enabled: bool,
        allow_spawned: bool,
        background: bool,
    ) -> Result<TurnAccepted, EngineError> {
        let thread = self.get_thread(thread_id)?; // 404 for unknown threads
        if !allow_spawned && self.subagent_is_read_only(&thread)? {
            return Err(EngineError::Conflict(
                "this subagent uses a read-only exploration, audit, or review mode".into(),
            ));
        }
        let (prepared, mut attachment_cleanup) = self.prepare_attachments(uploads)?;
        let attachments = prepared
            .iter()
            .map(|(attachment, _)| attachment.clone())
            .collect::<Vec<_>>();
        let attachment_rows = prepared
            .iter()
            .map(|(attachment, path)| (attachment.clone(), path.to_string_lossy().into_owned()))
            .collect::<Vec<_>>();
        let activity_publication = self.session_activity_publication.lock().unwrap();
        let _queue_mutation = self.prompt_queue_mutations.lock().unwrap();
        let mut active = self.active_threads.lock().unwrap();
        if self
            .deleting_sessions
            .lock()
            .unwrap()
            .contains(&thread.session_id)
        {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                thread.session_id
            )));
        }

        let position = self.store.next_queued_prompt_position(thread_id)?;
        let prompt = trouve_protocol::QueuedPrompt {
            id: format!("qp_{}", uuid::Uuid::new_v4().simple()),
            thread_id: thread_id.to_string(),
            position,
            content,
            background,
            attachments,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let mut visible_queue = self.store.queued_prompts(thread_id)?;
        visible_queue.push(prompt.clone());
        visible_queue.sort_by_key(|candidate| candidate.position);

        if active.contains_key(thread_id) {
            let cancelling = self
                .turn_cancels
                .lock()
                .unwrap()
                .get(thread_id)
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
            if cancelling {
                self.resume_after_cancel
                    .lock()
                    .unwrap()
                    .insert(thread_id.to_string());
            }
            let queued_prompt = prompt.clone();
            let result = self.store.accept_prompt_with_events(
                PromptAcceptance {
                    prompt,
                    tools_enabled,
                    attachments: attachment_rows,
                    claim_prompt_id: None,
                    expected_previous_turn: None,
                    staging_cleanup_claim: attachment_cleanup.claim(),
                },
                vec![(
                    Scope::Thread(thread_id.to_string()),
                    Event::QueueUpdated {
                        prompts: visible_queue,
                    },
                )],
            );
            drop(active);
            drop(activity_publication);
            if let Err(error) = result {
                return Err(error.into());
            }
            attachment_cleanup.disarm();
            return Ok(TurnAccepted {
                thread_id: thread_id.to_string(),
                turn: 0,
                queued: true,
                queued_prompt: Some(queued_prompt),
            });
        }

        let started_prompt = visible_queue.remove(0);
        let started_tools_enabled = if started_prompt.id == prompt.id {
            tools_enabled
        } else {
            self.store.queued_prompt_tools_enabled(&started_prompt.id)?
        };
        let previous_turn = self.store.last_turn(thread_id)?;
        let turn = previous_turn + 1;
        let session_woke = !active
            .values()
            .any(|session_id| session_id == &thread.session_id);
        let mut events = Vec::with_capacity(if session_woke { 4 } else { 3 });
        if session_woke {
            let workspace_id = self
                .store
                .session(&thread.session_id)?
                .map(|session| session.workspace_id)
                .unwrap_or_default();
            events.push((
                Scope::Server,
                Event::SessionActivity {
                    session_id: thread.session_id.clone(),
                    workspace_id,
                    active: true,
                },
            ));
        }
        events.push((
            Scope::Thread(thread_id.to_string()),
            Event::QueueUpdated {
                prompts: visible_queue,
            },
        ));
        events.extend(
            self.turn_shell_events(&thread, turn, &started_prompt, started_tools_enabled)?
                .into_iter()
                .map(|event| (Scope::Thread(thread_id.to_string()), event)),
        );

        active.insert(thread_id.to_string(), thread.session_id.clone());
        let cancel = self.register_cancel(thread_id);
        let accepted = self.store.accept_prompt_with_events(
            PromptAcceptance {
                prompt,
                tools_enabled,
                attachments: attachment_rows,
                claim_prompt_id: Some(started_prompt.id.clone()),
                expected_previous_turn: Some(previous_turn),
                staging_cleanup_claim: attachment_cleanup.claim(),
            },
            events,
        );
        if let Err(error) = accepted {
            active.remove(thread_id);
            self.clear_cancel(thread_id);
            drop(active);
            drop(activity_publication);
            return Err(error.into());
        }
        drop(active);
        drop(activity_publication);
        attachment_cleanup.disarm();
        self.spawn_claimed_prompt(thread, turn, started_prompt, cancel, true);
        Ok(TurnAccepted {
            thread_id: thread_id.to_string(),
            turn,
            queued: false,
            queued_prompt: None,
        })
    }

    /// Decode uploads and write their opaque files without touching SQLite.
    /// Ordinary sends pass these records to the event writer so attachment
    /// indexing and prompt acceptance share one durable transaction.
    fn prepare_attachments(
        &self,
        uploads: Vec<trouve_protocol::AttachmentUpload>,
    ) -> Result<
        (
            Vec<(trouve_protocol::Attachment, PathBuf)>,
            PreparedAttachmentCleanup,
        ),
        EngineError,
    > {
        use base64::Engine as _;
        validate_attachment_uploads(&uploads)?;
        let mut decoded = Vec::new();
        let dir = self.data_dir.join("attachments");
        for up in uploads {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(up.data.as_bytes())
                .map_err(|e| {
                    EngineError::BadRequest(format!("attachment {}: invalid base64: {e}", up.name))
                })?;
            debug_assert!(!bytes.is_empty() && bytes.len() <= MAX_ATTACHMENT_BYTES);
            let id = format!("at_{}", uuid::Uuid::new_v4().simple());
            // Store under the opaque id; keep the (sanitized) extension so
            // tools and vendor CLIs sniff the type naturally.
            let ext = Path::new(&up.name)
                .extension()
                .and_then(|e| e.to_str())
                .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
                .map(|e| format!(".{}", e.to_ascii_lowercase()))
                .unwrap_or_default();
            let path = dir.join(format!("{id}{ext}"));
            let attachment = trouve_protocol::Attachment {
                id,
                name: up.name,
                mime: up.mime,
                size_bytes: bytes.len() as u64,
            };
            decoded.push((attachment, path, bytes));
        }
        let paths = decoded
            .iter()
            .map(|(_, path, _)| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        // Record every intended path before the first write. If the process
        // exits after creation but before prompt ownership commits, the
        // durable job becomes retryable when its preparation lease expires.
        let cleanup_job = self.store.stage_attachment_cleanup(paths)?;
        let mut cleanup = PreparedAttachmentCleanup::new(
            self.store.clone(),
            self.executor.clone(),
            dir.clone(),
            decoded.iter().map(|(_, path, _)| path.clone()).collect(),
            cleanup_job.and_then(|job| job.claim()),
        );
        for (_, path, bytes) in &decoded {
            self.executor
                .prepare_attachment_file(&dir, path, bytes)
                .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        }
        let prepared = decoded
            .into_iter()
            .map(|(attachment, path, _)| (attachment, path))
            .collect();
        // The caller disarms only after deleting the staging job in the same
        // transaction that establishes durable attachment ownership.
        cleanup.armed = true;
        Ok((prepared, cleanup))
    }

    /// Metadata and verified bytes for one attachment (serves
    /// `GET /v1/attachments/{id}`).
    pub async fn attachment(
        &self,
        id: &str,
    ) -> Result<(trouve_protocol::Attachment, Vec<u8>), EngineError> {
        let (attachment, path) = self
            .store
            .attachment(id)?
            .ok_or_else(|| EngineError::NotFound(format!("attachment {id}")))?;
        let bytes = self
            .executor
            .read_attachment_file(
                &self.data_dir.join("attachments"),
                Path::new(&path),
                attachment.size_bytes,
            )
            .await
            .map_err(|error| EngineError::NotFound(format!("attachment {id}: {error}")))?;
        Ok((attachment, bytes))
    }

    /// Resolve durable rows to executor inputs without inspecting or opening
    /// the host filesystem in the engine.
    fn resolve_attachments(
        &self,
        attachments: &[trouve_protocol::Attachment],
    ) -> Result<Vec<AttachmentMaterializationFile>, EngineError> {
        attachments
            .iter()
            .map(|attachment| match self.store.attachment(&attachment.id)? {
                Some((metadata, path)) if metadata == *attachment => {
                    Ok(AttachmentMaterializationFile {
                        attachment: metadata,
                        source: PathBuf::from(path),
                    })
                }
                Some(_) => Err(EngineError::Conflict(format!(
                    "attachment {} metadata changed after prompt acceptance",
                    attachment.id
                ))),
                None => Err(EngineError::NotFound(format!(
                    "attachment {}",
                    attachment.id
                ))),
            })
            .collect()
    }

    async fn materialize_attachments_for_turn(
        &self,
        session: &Session,
        attachments: &[trouve_protocol::Attachment],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<MaterializedAttachment>, EngineError> {
        let files = self.resolve_attachments(attachments)?;
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let lane = self.tool_execution_lock(&session.id);
        let _mutation = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(EngineError::Conflict("turn cancelled".into())),
            permit = lane.write_owned() => permit,
        };
        self.executor
            .materialize_attachments(&AttachmentMaterialization {
                source_root: self.data_dir.join("attachments"),
                managed_worktree_root: self.data_dir.join("worktrees"),
                worktree: PathBuf::from(&session.worktree_path),
                files,
                cancel: cancel.clone(),
            })
            .await
            .map_err(|error| EngineError::Internal(anyhow!(error)))
    }

    /// Publish the thread's current queue on its event stream.
    fn emit_queue(&self, thread_id: &str) -> Result<(), EngineError> {
        let prompts = self.store.queued_prompts(thread_id)?;
        self.store.append_event(
            Scope::Thread(thread_id.to_string()),
            Event::QueueUpdated { prompts },
        )?;
        Ok(())
    }

    // --- prompt queue ----------------------------------------------------

    pub fn list_queued_prompts(
        &self,
        thread_id: &str,
    ) -> Result<Vec<trouve_protocol::QueuedPrompt>, EngineError> {
        self.get_thread(thread_id)?;
        Ok(self.store.queued_prompts(thread_id)?)
    }

    pub fn update_queued_prompt(
        &self,
        prompt_id: &str,
        request: trouve_protocol::UpdateQueuedPromptRequest,
    ) -> Result<(), EngineError> {
        let _queue_mutation = self.prompt_queue_mutations.lock().unwrap();
        let thread_id = self
            .store
            .queued_prompt_thread(prompt_id)?
            .ok_or_else(|| EngineError::NotFound(format!("queued prompt {prompt_id}")))?;
        let thread = self.get_thread(&thread_id)?;
        let deleting = self.deleting_sessions.lock().unwrap();
        if deleting.contains(&thread.session_id) {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                thread.session_id
            )));
        }
        let prompt = self
            .store
            .queued_prompts(&thread_id)?
            .into_iter()
            .find(|prompt| prompt.id == prompt_id)
            .ok_or_else(|| EngineError::NotFound(format!("queued prompt {prompt_id}")))?;

        let original_attachments = prompt.attachments;
        let mut attachments = if let Some(retained_ids) = request.retained_attachment_ids {
            let by_id: std::collections::HashMap<_, _> = original_attachments
                .iter()
                .cloned()
                .map(|attachment| (attachment.id.clone(), attachment))
                .collect();
            let mut retained = Vec::with_capacity(retained_ids.len());
            let mut seen = std::collections::HashSet::with_capacity(retained_ids.len());
            for id in retained_ids {
                if !seen.insert(id.clone()) {
                    return Err(EngineError::BadRequest(format!(
                        "queued attachment {id} was retained more than once"
                    )));
                }
                retained.push(by_id.get(&id).cloned().ok_or_else(|| {
                    EngineError::BadRequest(format!(
                        "attachment {id} does not belong to queued prompt {prompt_id}"
                    ))
                })?);
            }
            retained
        } else {
            original_attachments.clone()
        };
        let retained_ids = attachments
            .iter()
            .map(|attachment| attachment.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let removed_ids = original_attachments
            .iter()
            .filter(|attachment| !retained_ids.contains(attachment.id.as_str()))
            .map(|attachment| attachment.id.clone())
            .collect::<Vec<_>>();

        let (prepared, mut added_cleanup) = self.prepare_attachments(request.attachments)?;
        attachments.extend(prepared.iter().map(|(attachment, _)| attachment.clone()));
        let added_rows = prepared
            .iter()
            .map(|(attachment, path)| (attachment.clone(), path.to_string_lossy().into_owned()))
            .collect::<Vec<_>>();
        let Some(cleanup_job) = self.store.update_queued_prompt_attachments(
            prompt_id,
            &request.content,
            &attachments,
            &added_rows,
            &removed_ids,
            added_cleanup.claim().as_ref(),
        )?
        else {
            return Err(EngineError::NotFound(format!("queued prompt {prompt_id}")));
        };
        added_cleanup.disarm();
        if let Some(cleanup_job) = cleanup_job {
            self.schedule_artifact_cleanup(cleanup_job);
        }
        let result = self.emit_queue(&thread_id);
        drop(deleting);
        result
    }

    pub fn delete_queued_prompt(&self, prompt_id: &str) -> Result<(), EngineError> {
        let _queue_mutation = self.prompt_queue_mutations.lock().unwrap();
        let thread_id = self
            .store
            .queued_prompt_thread(prompt_id)?
            .ok_or_else(|| EngineError::NotFound(format!("queued prompt {prompt_id}")))?;
        let thread = self.get_thread(&thread_id)?;
        let deleting = self.deleting_sessions.lock().unwrap();
        if deleting.contains(&thread.session_id) {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                thread.session_id
            )));
        }
        let Some((thread_id, cleanup_job)) =
            self.store.delete_queued_prompt_attachments(prompt_id)?
        else {
            return Err(EngineError::NotFound(format!("queued prompt {prompt_id}")));
        };
        if let Some(cleanup_job) = cleanup_job {
            self.schedule_artifact_cleanup(cleanup_job);
        }
        let result = self.emit_queue(&thread_id);
        drop(deleting);
        result
    }

    /// Apply a full new order for the thread's queue. `ids` must name every
    /// currently queued prompt exactly once.
    pub fn reorder_queue(&self, thread_id: &str, ids: &[String]) -> Result<(), EngineError> {
        let _queue_mutation = self.prompt_queue_mutations.lock().unwrap();
        let thread = self.get_thread(thread_id)?;
        let deleting = self.deleting_sessions.lock().unwrap();
        if deleting.contains(&thread.session_id) {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                thread.session_id
            )));
        }
        if !self.store.reorder_queued_prompts(thread_id, ids)? {
            return Err(EngineError::Conflict(
                "queue changed while reordering; refresh and retry".into(),
            ));
        }
        let result = self.emit_queue(thread_id);
        drop(deleting);
        result
    }

    /// Prioritize one queued prompt and run it next. If a turn still owns the
    /// thread, interrupt that turn and explicitly resume the dispatcher after
    /// its terminal event; otherwise claim and start the selected prompt now.
    pub fn dispatch_queued_prompt(
        self: &Arc<Self>,
        prompt_id: &str,
    ) -> Result<TurnAccepted, EngineError> {
        let thread_id = self
            .store
            .queued_prompt_thread(prompt_id)?
            .ok_or_else(|| EngineError::NotFound(format!("queued prompt {prompt_id}")))?;
        let thread = self.get_thread(&thread_id)?;
        let activity_publication = self.session_activity_publication.lock().unwrap();
        let _queue_mutation = self.prompt_queue_mutations.lock().unwrap();
        let mut active = self.active_threads.lock().unwrap();
        if self
            .deleting_sessions
            .lock()
            .unwrap()
            .contains(&thread.session_id)
        {
            return Err(EngineError::Conflict(format!(
                "session {} is being deleted",
                thread.session_id
            )));
        }

        let turn_running = active.contains_key(&thread_id);
        let turn_cancel = if turn_running {
            Some(
                self.turn_cancels
                    .lock()
                    .unwrap()
                    .get(&thread_id)
                    .cloned()
                    .ok_or_else(|| {
                        EngineError::Conflict(format!(
                            "thread {thread_id} is between interruptible turns"
                        ))
                    })?,
            )
        } else {
            None
        };
        let original_order = self
            .store
            .queued_prompts(&thread_id)?
            .into_iter()
            .map(|prompt| prompt.id)
            .collect::<Vec<_>>();
        let prompt = self
            .store
            .prioritize_queued_prompt(prompt_id, !turn_running)?
            .ok_or_else(|| EngineError::NotFound(format!("queued prompt {prompt_id}")))?;
        if turn_running {
            // The dispatcher owns the active-thread claim until it publishes
            // the cancelled turn. Mark the explicit resume before tripping
            // the token so it will claim the newly prioritized prompt next.
            self.resume_after_cancel
                .lock()
                .unwrap()
                .insert(thread_id.clone());
            if let Err(error) = self.emit_queue(&thread_id) {
                self.resume_after_cancel.lock().unwrap().remove(&thread_id);
                if !self
                    .store
                    .reorder_queued_prompts(&thread_id, &original_order)?
                {
                    return Err(EngineError::Conflict(format!(
                        "queue changed while rolling back failed priority publication for {prompt_id}"
                    )));
                }
                return Err(error);
            }
            drop(active);
            drop(activity_publication);
            turn_cancel
                .expect("an active thread must have an interrupt token")
                .cancel();
            return Ok(TurnAccepted {
                thread_id,
                turn: 0,
                queued: true,
                queued_prompt: Some(prompt),
            });
        }

        let session_woke = !active.values().any(|session| *session == thread.session_id);
        active.insert(thread_id.clone(), thread.session_id.clone());
        let cancel = self.register_cancel(&thread_id);
        drop(active);
        let turn = match self.launch_claimed_prompt(
            thread,
            prompt,
            session_woke,
            cancel,
            activity_publication,
        ) {
            Ok(turn) => turn,
            Err(error) => {
                if !self
                    .store
                    .reorder_queued_prompts(&thread_id, &original_order)?
                {
                    return Err(EngineError::Conflict(format!(
                        "queue changed while rolling back failed priority dispatch for {prompt_id}"
                    )));
                }
                return Err(error);
            }
        };
        Ok(TurnAccepted {
            thread_id,
            turn,
            queued: false,
            queued_prompt: None,
        })
    }

    /// Start draining the thread's queue if it's idle — the "Send now"
    /// affordance. Deliberately never called automatically at startup: a
    /// crash may have cut a turn short, and running the queue on top of
    /// half-finished work needs a human's judgment. (A failed turn likewise
    /// pauses its queue until the user kicks it.)
    /// Returns the turn number of the dispatched prompt, or `None` when a
    /// turn is already running or the queue is empty.
    pub fn dispatch_queue(self: &Arc<Self>, thread_id: &str) -> Result<Option<u64>, EngineError> {
        let thread = self.get_thread(thread_id)?;
        let activity_publication = self.session_activity_publication.lock().unwrap();
        let _queue_mutation = self.prompt_queue_mutations.lock().unwrap();
        // Claim the thread and take the queue front atomically so two
        // concurrent sends can't both start a dispatcher.
        let (prompt, session_woke, cancel) = {
            let mut active = self.active_threads.lock().unwrap();
            if self
                .deleting_sessions
                .lock()
                .unwrap()
                .contains(&thread.session_id)
            {
                return Err(EngineError::Conflict(format!(
                    "session {} is being deleted",
                    thread.session_id
                )));
            }
            if active.contains_key(thread_id) {
                // A send that races cancellation cleanup is an explicit
                // request to keep working. Remember it while holding the
                // active-thread lock so the cancelling dispatcher either
                // sees this marker or releases the claim before this send
                // retries dispatch below.
                let cancelling = self
                    .turn_cancels
                    .lock()
                    .unwrap()
                    .get(thread_id)
                    .is_some_and(tokio_util::sync::CancellationToken::is_cancelled);
                if cancelling {
                    self.resume_after_cancel
                        .lock()
                        .unwrap()
                        .insert(thread_id.to_string());
                }
                return Ok(None);
            }
            let Some(p) = self.store.claim_queued_prompt(thread_id)? else {
                return Ok(None);
            };
            let was_active = active.values().any(|s| *s == thread.session_id);
            active.insert(thread_id.to_string(), thread.session_id.clone());
            let cancel = self.register_cancel(thread_id);
            (p, !was_active, cancel)
        };
        self.launch_claimed_prompt(thread, prompt, session_woke, cancel, activity_publication)
            .map(Some)
    }

    /// Complete setup for a prompt already claimed under the active-thread
    /// lock and launch its queue-draining task.
    fn launch_claimed_prompt(
        self: &Arc<Self>,
        thread: Thread,
        prompt: trouve_protocol::QueuedPrompt,
        session_woke: bool,
        cancel: tokio_util::sync::CancellationToken,
        activity_publication: std::sync::MutexGuard<'_, ()>,
    ) -> Result<u64, EngineError> {
        let thread_id = thread.id.clone();
        if session_woke && let Err(error) = self.emit_session_activity(&thread.session_id, true) {
            let prompt_release = self.store.release_queued_prompt(&prompt.id);
            self.active_threads.lock().unwrap().remove(&thread_id);
            self.clear_cancel(&thread_id);
            drop(activity_publication);
            prompt_release?;
            return Err(error);
        }
        drop(activity_publication);
        // If setup fails after claiming, release the claim — otherwise the
        // thread stays "active" forever and can never dispatch again.
        if let Err(e) = self.emit_queue(&thread_id) {
            let _ = self.store.release_queued_prompt(&prompt.id);
            self.clear_cancel(&thread_id);
            self.release_thread(&thread_id)?;
            return Err(e);
        }
        let turn = match self.store.next_turn(&thread_id) {
            Ok(t) => t,
            Err(e) => {
                let _ = self.store.release_queued_prompt(&prompt.id);
                self.clear_cancel(&thread_id);
                self.release_thread(&thread_id)?;
                return Err(e.into());
            }
        };
        self.spawn_claimed_prompt(thread, turn, prompt, cancel, false);
        Ok(turn)
    }

    fn spawn_claimed_prompt(
        self: &Arc<Self>,
        thread: Thread,
        turn: u64,
        prompt: trouve_protocol::QueuedPrompt,
        cancel: tokio_util::sync::CancellationToken,
        prompt_persisted: bool,
    ) {
        let automated_review_tool_budget = self
            .automated_review_tool_budgets
            .claim_dispatch(&thread.id);
        let engine = self.clone();
        tokio::spawn(async move {
            let thread_id = thread.id.clone();
            if let Err(error) = engine
                .drain_queue(
                    thread,
                    turn,
                    prompt,
                    cancel,
                    prompt_persisted,
                    automated_review_tool_budget,
                )
                .await
            {
                // A terminal or activity event failed to persist. The
                // transition keeps or restores the active claim so no later
                // turn can overtake the unrecorded state.
                tracing::error!("turn dispatcher for {thread_id} retained its claim: {error:#}");
            }
        });
    }

    /// Run `content` as `turn`, then keep pulling queued prompts until the
    /// queue is empty or a turn fails (a failure pauses the queue so a
    /// persistent error can't burn every queued prompt).
    async fn drain_queue(
        self: &Arc<Self>,
        thread: Thread,
        turn: u64,
        prompt: trouve_protocol::QueuedPrompt,
        first_cancel: tokio_util::sync::CancellationToken,
        first_prompt_persisted: bool,
        mut automated_review_tool_budget: Option<AutomatedReviewToolBudgetGuard>,
    ) -> Result<()> {
        let mut thread = thread;
        let mut turn = turn;
        let mut prompt = prompt;
        let mut turn_cancel = Some(first_cancel);
        let mut shell_persisted = first_prompt_persisted;
        loop {
            let cancel = turn_cancel
                .take()
                .expect("an active queue prompt must have a cancellation token");
            let prompt_persisted = AtomicBool::new(shell_persisted);
            let result = std::panic::AssertUnwindSafe(self.run_turn(
                &thread,
                turn,
                &prompt,
                cancel.clone(),
                &prompt_persisted,
            ))
            .catch_unwind()
            .await;
            let result = match result {
                Ok(result) => result,
                Err(_) => {
                    tracing::error!("turn {turn} of {} panicked", thread.id);
                    self.store
                        .append_event(
                            Scope::Thread(thread.id.clone()),
                            Event::TurnFailed {
                                turn,
                                error: "internal error".into(),
                            },
                        )
                        .with_context(|| {
                            format!(
                                "persisting failure after panic in turn {turn} of {}",
                                thread.id
                            )
                        })?;
                    let _ = self.store.release_queued_prompt(&prompt.id);
                    let _ = self.emit_queue(&thread.id);
                    self.clear_cancel(&thread.id);
                    self.release_thread(&thread.id)?;
                    return Ok(());
                }
            };
            let cancelled = cancel.is_cancelled();
            if !cancelled {
                let outcome_error = result.as_ref().err().map(ToString::to_string);
                self.turn_scheduler
                    .record_outcome(&thread.model, outcome_error.as_deref());
            }
            // Cancellation wins a race with startup/stream errors only after
            // run_turn has returned, which is the adapter/tool acknowledgement
            // that its bounded cleanup is complete. Never publish both failed
            // and cancelled terminal states for one turn.
            if cancelled {
                if let Err(error) = &result {
                    tracing::warn!(
                        "turn {turn} of {} returned during cancellation cleanup: {error}",
                        thread.id
                    );
                }
                // A cancellation can arrive before run_turn consumes the
                // claimed queue row. Remove it here as an idempotent fallback.
                let _ = self.store.finish_queued_prompt(&prompt.id);
                let mut terminal_events = Vec::with_capacity(3);
                if !prompt_persisted.load(Ordering::Acquire) {
                    // Cancellation becomes available as soon as dispatch is
                    // claimed, before provider discovery or scheduler waits
                    // can publish the display events. Preserve the claimed
                    // prompt even when cancellation wins that startup race.
                    terminal_events.extend([
                        Event::TurnStarted {
                            turn,
                            mode: thread.mode.clone(),
                            model: thread.model.clone(),
                            thinking_level: None,
                            supports_steering: false,
                        },
                        Event::UserMessage {
                            turn,
                            content: prompt.content.clone(),
                            attachments: prompt.attachments.clone(),
                            background: prompt.background,
                        },
                    ]);
                }
                terminal_events.push(Event::TurnCancelled { turn });
                self.store
                    .append_events_async(Scope::Thread(thread.id.clone()), terminal_events)
                    .await
                    .with_context(|| {
                        format!("persisting cancellation for turn {turn} of {}", thread.id)
                    })?;
                let resume = self.finish_interrupted_turn(&thread.id)?;
                if !resume {
                    return Ok(());
                }
            }
            let resume_after_failure = if !cancelled && let Err(e) = result {
                tracing::error!("turn {turn} of {} failed: {e}", thread.id);
                self.store
                    .append_event(
                        Scope::Thread(thread.id.clone()),
                        Event::TurnFailed {
                            turn,
                            error: e.to_string(),
                        },
                    )
                    .with_context(|| {
                        format!("persisting failure for turn {turn} of {}", thread.id)
                    })?;
                let _ = self.store.release_queued_prompt(&prompt.id);
                let _ = self.emit_queue(&thread.id);
                let resume = self.finish_interrupted_turn(&thread.id)?;
                if !resume {
                    return Ok(());
                }
                true
            } else {
                false
            };
            // The first dispatched turn now has a durable terminal outcome,
            // or acknowledged cancellation cleanup. Release its disposable
            // review policy before an unrelated queued prompt can start.
            drop(automated_review_tool_budget.take());
            // Pop the next prompt; releasing the claim and inspecting the
            // queue must be atomic against concurrent send_message calls.
            let (next, next_cancel) = {
                let _activity_publication = self.session_activity_publication.lock().unwrap();
                let _queue_mutation = self.prompt_queue_mutations.lock().unwrap();
                let (next, next_cancel, idle_session) = {
                    let mut active = self.active_threads.lock().unwrap();
                    if !resume_after_failure && !cancelled {
                        self.clear_cancel(&thread.id);
                    }
                    match self.store.claim_queued_prompt(&thread.id) {
                        Ok(Some(prompt)) => {
                            let cancel = self.register_cancel(&thread.id);
                            (Some(prompt), Some(cancel), None)
                        }
                        _ => (
                            None,
                            None,
                            Self::remove_thread_claim(&mut active, &thread.id),
                        ),
                    }
                };
                self.publish_idle_or_restore(&thread.id, idle_session)?;
                (next, next_cancel)
            };
            let Some(next) = next else { return Ok(()) };
            turn_cancel = next_cancel;
            let _ = self.emit_queue(&thread.id);
            // Thread settings may have changed between turns.
            if let Ok(t) = self.get_thread(&thread.id) {
                thread = t;
            }
            turn = match self.store.next_turn(&thread.id) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("queue for {} stopped: {e}", thread.id);
                    let _ = self.store.release_queued_prompt(&next.id);
                    let _ = self.emit_queue(&thread.id);
                    self.release_thread(&thread.id)?;
                    return Ok(());
                }
            };
            prompt = next;
            shell_persisted = false;
        }
    }

    /// Drop a thread's dispatcher claim; when it was the session's last
    /// active thread, announce the session going idle.
    fn release_thread(&self, thread_id: &str) -> Result<(), EngineError> {
        let _activity_publication = self.session_activity_publication.lock().unwrap();
        let idle_session = {
            let mut active = self.active_threads.lock().unwrap();
            Self::remove_thread_claim(&mut active, thread_id)
        };
        self.publish_idle_or_restore(thread_id, idle_session)
    }

    /// Remove a claim and return its session only when it was the session's
    /// last active thread. A missing claim never produces an idle transition.
    fn remove_thread_claim(
        active: &mut std::collections::HashMap<String, String>,
        thread_id: &str,
    ) -> Option<String> {
        active
            .remove(thread_id)
            .filter(|session| !active.values().any(|candidate| candidate == session))
    }

    /// Publish an idle transition, restoring its claim when persistence fails.
    /// The caller must hold `session_activity_publication`.
    fn publish_idle_or_restore(
        &self,
        thread_id: &str,
        idle_session: Option<String>,
    ) -> Result<(), EngineError> {
        let Some(session_id) = idle_session else {
            return Ok(());
        };
        if let Err(error) = self.emit_session_activity(&session_id, false) {
            self.active_threads
                .lock()
                .unwrap()
                .insert(thread_id.to_string(), session_id);
            return Err(error);
        }
        Ok(())
    }

    /// Register a fresh cancellation token for a turn about to run.
    fn register_cancel(&self, thread_id: &str) -> tokio_util::sync::CancellationToken {
        self.bridged_tool_owners.begin_root(thread_id);
        let token = tokio_util::sync::CancellationToken::new();
        self.turn_cancels
            .lock()
            .unwrap()
            .insert(thread_id.to_string(), token.clone());
        token
    }

    fn clear_cancel(&self, thread_id: &str) {
        self.turn_cancels.lock().unwrap().remove(thread_id);
        self.resume_after_cancel.lock().unwrap().remove(thread_id);
        self.bridged_tool_owners.clear_root(thread_id);
    }

    /// Finish a cancelled or failed turn while coordinating with dispatches
    /// waiting on the same active-thread claim. Returns true when one of
    /// those dispatches requested that the queue continue draining.
    fn finish_interrupted_turn(&self, thread_id: &str) -> Result<bool, EngineError> {
        // Lock ordering matches `dispatch_queue`: activity publication,
        // active thread, cancel token, then resume marker.
        let _activity_publication = self.session_activity_publication.lock().unwrap();
        let (resume, idle_session) = {
            let mut active = self.active_threads.lock().unwrap();
            let mut cancels = self.turn_cancels.lock().unwrap();
            let mut resumes = self.resume_after_cancel.lock().unwrap();
            let resume = resumes.remove(thread_id);
            cancels.remove(thread_id);
            let idle_session = if resume {
                None
            } else {
                Self::remove_thread_claim(&mut active, thread_id)
            };
            (resume, idle_session)
        };
        self.bridged_tool_owners.clear_root(thread_id);
        self.publish_idle_or_restore(thread_id, idle_session)?;
        Ok(resume)
    }

    /// Interrupt the turn currently running on a thread. The request only
    /// trips its shared token; the dispatcher publishes `turn.cancelled`
    /// after provider/tool cleanup acknowledges that no stale work can race
    /// a replacement turn. No-op error when the thread has no running turn.
    pub fn cancel_turn(&self, thread_id: &str) -> Result<(), EngineError> {
        match self.turn_cancels.lock().unwrap().get(thread_id) {
            Some(token) => {
                token.cancel();
                Ok(())
            }
            None => Err(EngineError::BadRequest(format!(
                "no running turn to cancel on thread {thread_id}"
            ))),
        }
    }

    pub(crate) fn begin_automated_review_tool_budget(
        &self,
        thread_id: &str,
        limit: u64,
    ) -> Result<AutomatedReviewToolBudgetGuard> {
        self.automated_review_tool_budgets.arm(thread_id, limit)
    }

    /// Server-scope `session.activity` event — session lists light up (or
    /// dim) their indicator without refetching.
    fn emit_session_activity(&self, session_id: &str, active: bool) -> Result<(), EngineError> {
        let workspace_id = self
            .store
            .session(session_id)?
            .map(|s| s.workspace_id)
            .unwrap_or_default();
        self.store.append_event(
            Scope::Server,
            Event::SessionActivity {
                session_id: session_id.to_string(),
                workspace_id,
                active,
            },
        )?;
        Ok(())
    }

    async fn run_turn(
        self: &Arc<Self>,
        thread: &Thread,
        turn: u64,
        prompt: &trouve_protocol::QueuedPrompt,
        cancel: tokio_util::sync::CancellationToken,
        prompt_persisted: &AtomicBool,
    ) -> Result<()> {
        let content = prompt.content.clone();
        let attachments = prompt.attachments.clone();
        let tools_enabled = self.store.queued_prompt_tools_enabled(&prompt.id)?;
        if !prompt_persisted.load(Ordering::Acquire) {
            self.store
                .append_events_async(
                    Scope::Thread(thread.id.clone()),
                    self.turn_shell_events(thread, turn, prompt, tools_enabled)?,
                )
                .await?;
            prompt_persisted.store(true, Ordering::Release);
        }
        let session = self
            .store
            .session(&thread.session_id)?
            .context("session vanished")?;
        let ws = self
            .store
            .workspace(&session.workspace_id)?
            .context("workspace vanished")?;
        let scope = Scope::Thread(thread.id.clone());
        let worktree = PathBuf::from(&session.worktree_path);
        let canonical_worktree = worktree.canonicalize()?;
        let ctx = ToolCtx {
            cancel: cancel.clone(),
            worktree: worktree.clone(),
            canonical_worktree: Some(canonical_worktree),
            read_only_roots: crate::skills::trusted_read_roots(
                self.config_dir.as_deref(),
                Some(Path::new(&ws.path)),
            )
            .into(),
            thread_id: thread.id.clone(),
            todos: Arc::new(Mutex::new(thread.todos.clone())),
            config_dir: self.config_dir.clone(),
            workspace_root: Some(PathBuf::from(&ws.path)),
            edit_strategy: edit_strategy_for_model(&thread.model),
            background_mutation_lease: None,
        };

        let all_modes = self.resolve_personas(Some(Path::new(&ws.path)))?;
        let mut mode = personas::find_persona(&all_modes, &thread.mode)
            .cloned()
            .unwrap_or_else(personas::fallback_persona);
        let background = self.store.is_code_review_thread(&thread.id)?;
        if background {
            mode = personas::secure_automated_review_persona(mode);
        }
        // A turn owns only a shared session lifecycle lease. Sibling turns in
        // the same worktree may reason and invoke tools concurrently; the
        // narrower tool-execution lane is the authority that serializes
        // actual mutations. Exclusive lifecycle operations still wait for
        // every active turn.
        let session_lifecycle = self.session_lock(&session.id);
        let _session_lifecycle_guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            guard = session_lifecycle.read() => guard,
        };
        let turn_capacity = self
            .turn_scheduler
            .acquire(&thread.model, background, &cancel)
            .await?;
        if background
            && let Some(progress) = self
                .store
                .set_code_review_task_provider_wait(&thread.id, turn_capacity.wait_ms)?
        {
            self.emit_code_review_task_progress(progress).await?;
        }
        self.store
            .append_event_async(
                scope.clone(),
                Event::TurnCapacityAcquired {
                    turn,
                    wait_ms: turn_capacity.wait_ms,
                    background,
                },
            )
            .await?;

        let _turn_capacity = turn_capacity;

        // External agent backend? The vendor harness owns the loop; we
        // stream its events and bridge approvals. The shared lifecycle lease
        // stays held; mutation tools take the exclusive execution lane.
        if let Some((backend_id, backend, model_name)) = self.backend_for(&thread.model) {
            return self
                .run_backend_turn(
                    &session,
                    thread,
                    turn,
                    &mode,
                    &backend_id,
                    backend,
                    model_name,
                    content,
                    attachments,
                    cancel,
                    &prompt.id,
                    tools_enabled,
                    prompt.background,
                )
                .await;
        }

        let (provider, model_name) = self
            .resolve_provider(&thread.model)
            .map_err(|e| anyhow!(e.to_string()))?;
        let mut model_options = self.store.thread_model_options(&thread.id)?;
        let model_catalog = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            models = provider.list_models() => models,
        };
        let selected_model = model_catalog.iter().find(|m| m.id == thread.model);
        normalize_thinking_option(&mut model_options, selected_model);

        // Compact the transcript when it nears the model's context window,
        // before this turn's user message joins it (the stored transcript —
        // the event above is display-only).
        if let Err(e) = self
            .maybe_compact(thread, turn, &provider, &model_name, &cancel)
            .await
        {
            // Compaction is best-effort; the turn proceeds with full history.
            tracing::warn!("compaction failed for {}: {e}", thread.id);
        }
        // Native providers speak text-only; every attachment (images
        // included) becomes a path reference the model's file tools can
        // follow. Copy them into the worktree first: the file tools reject
        // absolute paths (the sandbox), so a data-dir path the model can't
        // open is useless — a worktree-relative copy is reachable.
        let materialized = self
            .materialize_attachments_for_turn(&session, &attachments, &cancel)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let prompt_files = materialized
            .iter()
            .map(|file| (file.attachment.clone(), file.relative_path.clone()))
            .collect::<Vec<_>>();
        let content = annotate_attachments(content, &prompt_files);
        self.store
            .append_message(&thread.id, &serde_json::to_value(Message::User(content))?)?;
        if !self.store.finish_queued_prompt(&prompt.id)? {
            bail!("queued prompt {} vanished before turn start", prompt.id);
        }

        // Tool policy: empty allowed_tools = all registered tools. Engine-
        // served tools are added only when tools_enabled is true; tool-free
        // JSON-repair turns receive no tools.
        let mut specs: Vec<ToolSpec> = if tools_enabled {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("turn cancelled"),
                specs = self.executor.specs(&ctx) => specs,
            }
            .into_iter()
            .filter(|spec| personas::tool_allowed(&mode, &spec.name))
            .collect()
        } else {
            Vec::new()
        };
        if tools_enabled && personas::tool_allowed(&mode, "ask_question") {
            specs.push(ask_question_spec());
        }
        if tools_enabled && personas::tool_allowed(&mode, "search_transcript") {
            specs.push(search_transcript_spec());
        }
        // Recursive spawn tools remain bounded by the durable tree depth and
        // also respect the mode policy, so restrictive/read-only personas that
        // do not list them cannot create child agents.
        if tools_enabled && self.thread_can_spawn_subagents(&thread.id)? {
            if personas::tool_allowed(&mode, "spawn_thread") {
                specs.push(spawn_thread_spec());
            }
            if personas::tool_allowed(&mode, "spawn_session") {
                specs.push(spawn_session_spec());
            }
            if personas::tool_allowed(&mode, "spawn_output") {
                specs.push(spawn_output_spec());
            }
        }

        let mut system =
            context::system_prompt(&mode, self.config_dir.as_deref(), Path::new(&ws.path));
        if background {
            // Context assembly intentionally layers trusted workspace
            // instructions after the persona. Repeat the immutable review
            // guard last so no configurable layer can weaken it.
            personas::append_automated_review_security_prompt(&mut system);
        }
        let live_models = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            models = provider.list_models() => models,
        };
        let known_models = provider.models();
        let pricing_model = live_models
            .iter()
            .chain(known_models.iter())
            .find(|m| m.id == thread.model)
            .cloned();
        let price_usage = |usage: &mut Usage| {
            if usage.cost_usd.is_none() {
                usage.cost_usd = pricing_model.as_ref().and_then(|model| {
                    self.model_catalog.cost_usd(
                        model,
                        usage.input_tokens,
                        usage.cached_input_tokens,
                        usage.output_tokens,
                    )
                });
            }
        };
        let mut usage_total = Usage::default();
        let mut accumulate_usage = |usage: &Usage| {
            usage_total.input_tokens += usage.input_tokens;
            usage_total.output_tokens += usage.output_tokens;
            usage_total.cached_input_tokens += usage.cached_input_tokens;
            if let Some(cost) = usage.cost_usd {
                usage_total.cost_usd = Some(usage_total.cost_usd.unwrap_or(0.0) + cost);
            }
            if usage.context_window.is_some() {
                usage_total.context_window = usage.context_window;
            }
        };
        // The last request's provider-authoritative context measurement.
        // Summing per-iteration inputs (usage_total) would over-count a
        // multi-tool turn many-fold; the final request carries the whole
        // transcript, so its context size is the useful value.
        let mut context_input_tokens = 0u64;
        // Becomes false when the loop ends because the model stopped calling
        // tools (or was cancelled); stays true only if we exhaust the
        // iteration budget mid-work, which we then surface to the user.
        let mut hit_iteration_limit = true;

        for _iteration in 0..MAX_ITERATIONS {
            if cancel.is_cancelled() {
                hit_iteration_limit = false;
                break;
            }
            // Rebuild the transcript each iteration; the store is the truth.
            let mut messages = vec![Message::System(system.clone())];
            for payload in self.store.messages(&thread.id)? {
                messages.push(serde_json::from_value(payload)?);
            }
            // Repair any tool_calls left without results by a crash/restart
            // mid-turn (and drop empty assistant turns); providers reject a
            // dangling tool_use/tool_call, which would wedge the thread.
            let messages = sanitize_transcript(messages);

            let stream = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                stream = provider.stream_chat(&model_name, &messages, &specs, &model_options) => {
                    Some(stream.map_err(|e| anyhow!("provider error: {e}"))?)
                }
            };
            let Some(mut stream) = stream else {
                hit_iteration_limit = false;
                break;
            };
            stream = trouve_providers::coalesce_event_stream(stream);

            let mut text = String::new();
            let mut tool_calls = Vec::new();
            // Provider-native reasoning blocks (Anthropic signed thinking) to
            // persist and replay verbatim — Anthropic rejects a follow-up
            // tool-use turn whose thinking blocks aren't preserved.
            let mut reasoning: Vec<serde_json::Value> = Vec::new();
            let mut thinking_streamed = false;
            let mut pending_events = Vec::new();
            let mut persist_deadline = None;
            loop {
                let flush_at = persist_deadline.unwrap_or_else(Instant::now);
                let ev = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    _ = tokio::time::sleep_until(flush_at.into()), if persist_deadline.is_some() => {
                        flush_backend_event_batch(&self.store, &scope, &mut pending_events).await?;
                        persist_deadline = None;
                        continue;
                    }
                    ev = stream.next() => ev,
                };
                let Some(ev) = ev else { break };
                let event = match ev {
                    Ok(event) => event,
                    Err(error) => {
                        flush_backend_event_batch(&self.store, &scope, &mut pending_events).await?;
                        return Err(anyhow!("provider stream error: {error}"));
                    }
                };
                match event {
                    ProviderEvent::TextDelta(delta) => {
                        text.push_str(&delta);
                        pending_events.push(Event::AssistantDelta { turn, text: delta });
                    }
                    // Display-only; never joins the provider transcript.
                    ProviderEvent::ThinkingDelta(delta) => {
                        thinking_streamed = true;
                        pending_events.push(Event::AssistantThinking { turn, text: delta });
                    }
                    // Kept out of the UI (already streamed as ThinkingDelta);
                    // carried in the transcript for replay only.
                    ProviderEvent::Reasoning(block) => reasoning.push(block),
                    ProviderEvent::ToolCall(call) => tool_calls.push(call),
                    ProviderEvent::Completed { mut usage } => {
                        context_input_tokens = usage.context_input_tokens.unwrap_or_else(|| {
                            usage.input_tokens.saturating_add(usage.cached_input_tokens)
                        });
                        usage.context_input_tokens = Some(context_input_tokens);
                        price_usage(&mut usage);
                        accumulate_usage(&usage);
                        pending_events.push(Event::TurnUsageUpdated { turn, usage });
                    }
                }
                if pending_events.len() >= STREAM_EVENT_BATCH_MAX {
                    flush_backend_event_batch(&self.store, &scope, &mut pending_events).await?;
                    persist_deadline = None;
                } else if !pending_events.is_empty() && persist_deadline.is_none() {
                    persist_deadline = Some(Instant::now() + STREAM_EVENT_BATCH_WINDOW);
                }
            }
            if thinking_streamed {
                pending_events.push(Event::AssistantThinkingCompleted { turn });
            }
            flush_backend_event_batch(&self.store, &scope, &mut pending_events).await?;

            // Interrupted mid-stream: keep any streamed text for display, but
            // drop the (unexecuted) tool calls so we don't strand tool_use
            // without results, and stop the turn.
            if cancel.is_cancelled() {
                if !text.is_empty() {
                    self.store
                        .append_event_async(
                            scope.clone(),
                            Event::AssistantMessage {
                                turn,
                                content: text.clone(),
                            },
                        )
                        .await?;
                    self.store.append_message(
                        &thread.id,
                        &serde_json::to_value(Message::Assistant {
                            content: text,
                            tool_calls: Vec::new(),
                            reasoning,
                        })?,
                    )?;
                }
                hit_iteration_limit = false;
                break;
            }

            if !text.is_empty() {
                self.store
                    .append_event_async(
                        scope.clone(),
                        Event::AssistantMessage {
                            turn,
                            content: text.clone(),
                        },
                    )
                    .await?;
            }
            // Skip a fully-empty assistant message (no text, no tool calls —
            // e.g. a thinking-only or empty provider response): it serializes
            // to an empty content block that Anthropic rejects on the next
            // request, wedging the thread.
            if !tools_enabled && !tool_calls.is_empty() {
                tracing::warn!(
                    thread_id = %thread.id,
                    turn,
                    "provider requested a tool during a tool-free turn; ignoring the request"
                );
                tool_calls.clear();
            }
            normalize_tool_call_ids(&mut tool_calls);
            if !text.is_empty() || !tool_calls.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: tool_calls.clone(),
                        reasoning,
                    })?,
                )?;
            }

            if tool_calls.is_empty() {
                hit_iteration_limit = false;
                break;
            }

            // Providers may request independent calls in one response. Poll
            // them concurrently, but collect in provider order so the
            // transcript remains valid for APIs that require ordered
            // tool-result blocks. Tool events retain their real start and
            // completion ordering through the durable event log.
            let results = self
                .handle_tool_calls_parallel(
                    &session, thread, turn, &mode, &ctx, tool_calls, &cancel,
                )
                .await;
            for (call_id, result) in results {
                let (result_content, images) = result?;
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::ToolResult {
                        call_id,
                        content: result_content,
                        images,
                    })?,
                )?;
            }
        }

        // Truncated mid-work at the iteration budget: make one final
        // tool-free provider pass over the last tool results so the user gets
        // a truthful model-authored progress report rather than a completed
        // turn whose transcript ends at a tool result.
        if hit_iteration_limit && !cancel.is_cancelled() {
            let mut messages = vec![Message::System(system.clone())];
            for payload in self.store.messages(&thread.id)? {
                messages.push(serde_json::from_value(payload)?);
            }
            let mut messages = sanitize_transcript(messages);
            messages.push(Message::User(format!(
                "You reached the hard {MAX_ITERATIONS}-step limit for this turn. Do not call any \
                 more tools. Give the user a concise progress report based on the tool results \
                 above, clearly identify unfinished work, and ask them to continue in a new turn."
            )));
            let mut final_text = String::new();
            let mut final_reasoning = Vec::new();
            let final_stream = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                stream = provider.stream_chat(&model_name, &messages, &[], &model_options) => Some(stream),
            };
            match final_stream {
                None => {}
                Some(Ok(stream)) => {
                    let mut stream = trouve_providers::coalesce_event_stream(stream);
                    let mut thinking_streamed = false;
                    let mut pending_events = Vec::new();
                    let mut persist_deadline = None;
                    loop {
                        let flush_at = persist_deadline.unwrap_or_else(Instant::now);
                        let event = tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break,
                            _ = tokio::time::sleep_until(flush_at.into()), if persist_deadline.is_some() => {
                                flush_backend_event_batch(&self.store, &scope, &mut pending_events).await?;
                                persist_deadline = None;
                                continue;
                            }
                            event = stream.next() => match event {
                                Some(event) => event,
                                None => break,
                            },
                        };
                        match event {
                            Ok(ProviderEvent::TextDelta(delta)) => {
                                final_text.push_str(&delta);
                                pending_events.push(Event::AssistantDelta { turn, text: delta });
                            }
                            Ok(ProviderEvent::ThinkingDelta(delta)) => {
                                thinking_streamed = true;
                                pending_events.push(Event::AssistantThinking { turn, text: delta });
                            }
                            Ok(ProviderEvent::Reasoning(block)) => final_reasoning.push(block),
                            Ok(ProviderEvent::Completed { mut usage }) => {
                                context_input_tokens =
                                    usage.context_input_tokens.unwrap_or_else(|| {
                                        usage.input_tokens.saturating_add(usage.cached_input_tokens)
                                    });
                                usage.context_input_tokens = Some(context_input_tokens);
                                price_usage(&mut usage);
                                accumulate_usage(&usage);
                                pending_events.push(Event::TurnUsageUpdated { turn, usage });
                            }
                            // Tools are deliberately unavailable on this
                            // final pass. Ignore a non-conforming provider's
                            // request and fall back to the explicit note.
                            Ok(ProviderEvent::ToolCall(_)) => {}
                            Err(e) => {
                                tracing::warn!("iteration-limit summary failed: {e}");
                                break;
                            }
                        }
                        if pending_events.len() >= STREAM_EVENT_BATCH_MAX {
                            flush_backend_event_batch(&self.store, &scope, &mut pending_events)
                                .await?;
                            persist_deadline = None;
                        } else if !pending_events.is_empty() && persist_deadline.is_none() {
                            persist_deadline = Some(Instant::now() + STREAM_EVENT_BATCH_WINDOW);
                        }
                    }
                    if thinking_streamed {
                        pending_events.push(Event::AssistantThinkingCompleted { turn });
                    }
                    flush_backend_event_batch(&self.store, &scope, &mut pending_events).await?;
                }
                Some(Err(e)) => tracing::warn!("iteration-limit summary failed: {e}"),
            }
            if cancel.is_cancelled() {
                return Ok(());
            }
            if final_text.trim().is_empty() {
                final_text = format!(
                    "Reached the {MAX_ITERATIONS}-step limit for one turn and stopped mid-task. \
                     Send another message to continue."
                );
            }
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: final_text.clone(),
                    },
                )
                .await?;
            self.store.append_message(
                &thread.id,
                &serde_json::to_value(Message::Assistant {
                    content: final_text,
                    tool_calls: Vec::new(),
                    reasoning: final_reasoning,
                })?,
            )?;
        }

        if cancel.is_cancelled() {
            return Ok(());
        }
        usage_total.context_input_tokens = Some(context_input_tokens);
        self.store.record_usage(
            &session.id,
            &thread.id,
            turn,
            &thread.model,
            &usage_total,
            context_input_tokens,
        )?;

        // Read-only turns never snapshot: any dirty worktree state belongs to
        // a concurrent mutation-capable turn. Code turns checkpoint through
        // the same exclusive mutation lane as edits and Git commands.
        let checkpoint_id = if mode.read_only {
            None
        } else {
            self.maybe_checkpoint(&session, thread, turn, &cancel)
                .await?
        };
        if cancel.is_cancelled() {
            return Ok(());
        }
        self.store
            .append_event_async(
                scope,
                Event::TurnCompleted {
                    turn,
                    usage: usage_total,
                    checkpoint_id,
                },
            )
            .await?;
        Ok(())
    }

    /// Resolve a provider-qualified model id to a registered agent backend.
    fn backend_for(&self, model: &str) -> Option<(String, Arc<dyn AgentBackend>, String)> {
        let (backend_id, model_name) = model.split_once('/')?;
        let backend = self.backends.read().unwrap().get(backend_id).cloned()?;
        Some((backend_id.to_string(), backend, model_name.to_string()))
    }

    /// MCP tool-bridge config for a backend turn. Claude Code and Codex use
    /// the full bridge by default so mutation-capable work crosses the same
    /// ToolExecutor and per-session execution lane as native provider calls.
    /// Cursor receives a supplemental semantic-search bridge because ACP
    /// cannot disable its native tools. An explicit `tool_bridge = false`
    /// retains the vendor-native fallback where a full bridge is supported.
    fn mcp_bridge_for(
        &self,
        model: &str,
        thread_id: &str,
    ) -> Option<trouve_agents::McpBridgeConfig> {
        let backend_id = model.split_once('/')?.0;
        let (kind, configured_bridge_tools) = {
            let config = self.config.lock().unwrap();
            let pc = config.providers.get(backend_id)?;
            (pc.kind.clone(), pc.tool_bridge.unwrap_or(true))
        };
        if !matches!(
            kind.as_str(),
            "claude-cli" | "codex-app-server" | "cursor-cli"
        ) {
            return None;
        }
        // Cursor cannot suppress its native ACP tools. Give it only the
        // supplemental always-bridged search surface; Ask mode confines its
        // native tools to read-only operations.
        let bridge_tools = kind != "cursor-cli" && configured_bridge_tools;
        let Some(base_url) = self.base_url.read().unwrap().clone() else {
            tracing::warn!(
                "MCP bridge wanted for {backend_id} but the server base URL is unknown; \
                 running without it (approvals will fail in ask mode)"
            );
            return None;
        };
        // Codex and Cursor approvals are native RPCs; serving Claude's
        // permission-gate tool would only tempt those models to call it.
        let serve_approval = kind == "claude-cli";
        let claims = BridgeTicketClaims {
            bridge_tools,
            serve_approval,
            correlate_codex_owner: kind == "codex-app-server",
        };
        let ticket = self.bridge_ticket_for(thread_id, claims);
        let mut url = format!(
            "{}/internal/threads/{}/mcp?tools={}&approval={}&ticket={}",
            base_url.trim_end_matches('/'),
            thread_id,
            bridge_tools as u8,
            serve_approval as u8,
            ticket,
        );
        if let Some(token) = self.bridge_token.read().unwrap().as_deref() {
            url.push_str("&bridge_token=");
            url.push_str(token);
        }
        Some(trouve_agents::McpBridgeConfig {
            url,
            bridge_tools,
            // Claude built-ins stand down; Codex uses the same bridge while
            // its remaining built-ins are confined by a read-only sandbox.
            disallowed_tools: if bridge_tools && kind == "claude-cli" {
                [
                    "Bash",
                    "Edit",
                    "Write",
                    "MultiEdit",
                    "NotebookEdit",
                    "WebFetch",
                    "WebSearch",
                    "Read",
                    "Glob",
                    "Grep",
                    "Task",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect()
            } else {
                Vec::new()
            },
        })
    }

    // --- bridged tools (MCP tool bridge, Phase 6) -----------------------------

    /// Tool specs for a thread, as exposed to a bridged vendor agent
    /// (filtered by the thread's mode, same as native turns).
    pub async fn bridged_tool_specs(
        &self,
        thread_id: &str,
        bridge_tools: bool,
    ) -> Result<Vec<ToolSpec>, EngineError> {
        let (_, _, mode, ctx) = self.bridged_context(thread_id)?;
        let discovered = if bridge_tools {
            self.executor.specs(&ctx).await
        } else {
            // The minimal bridge must not consult external MCP discovery:
            // listing three native tools is not authority to launch trusted
            // workspace/user servers.
            self.executor.native_specs(&ctx).await
        };
        let mut specs: Vec<ToolSpec> = discovered
            .into_iter()
            .filter(|spec| {
                (bridge_tools || matches!(spec.name.as_str(), "search" | "find_related"))
                    && personas::tool_allowed(&mode, &spec.name)
            })
            .collect();
        if personas::tool_allowed(&mode, "ask_question") {
            specs.push(ask_question_spec());
        }
        if !bridge_tools {
            return Ok(specs);
        }
        if personas::tool_allowed(&mode, "search_transcript") {
            specs.push(search_transcript_spec());
        }
        // Recursive spawn tools use the same mode and depth policy as native
        // provider turns.
        if self.thread_can_spawn_subagents(thread_id)? {
            if personas::tool_allowed(&mode, "spawn_thread") {
                specs.push(spawn_thread_spec());
            }
            if personas::tool_allowed(&mode, "spawn_session") {
                specs.push(spawn_session_spec());
            }
            if personas::tool_allowed(&mode, "spawn_output") {
                specs.push(spawn_output_spec());
            }
        }
        Ok(specs)
    }

    /// Execute one tool call on behalf of a bridged vendor agent, through
    /// the same gate/approval/event chokepoint as native tool calls.
    pub async fn bridged_tool_call(
        self: &Arc<Self>,
        thread_id: &str,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, EngineError> {
        let cancel = self.active_bridge_cancel(thread_id)?;
        self.bridged_tool_call_for(thread_id, name, arguments, cancel)
            .await
    }

    /// Execute a Codex MCP call after correlating app-server's authoritative
    /// root-or-collaborator owner. Spawned Codex agents inherit the root MCP
    /// URL, so the URL path alone is not a safe persistence scope.
    pub async fn bridged_codex_tool_call(
        self: &Arc<Self>,
        root_thread_id: &str,
        vendor_thread_id: Option<&str>,
        vendor_call_id: Option<&str>,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, EngineError> {
        let vendor_thread_id = vendor_thread_id
            .filter(|thread_id| !thread_id.is_empty())
            .ok_or_else(|| {
                EngineError::BadRequest(
                    "Codex MCP metadata is missing the required _meta.threadId".into(),
                )
            })?;
        let root_cancel = self.active_bridge_cancel(root_thread_id)?;
        let owner_registration = self
            .bridged_tool_owners
            .register_vendor_owner(root_thread_id, vendor_thread_id);
        let owner_thread_id = match owner_registration {
            CodexVendorOwnerRegistration::Immediate(owner) => owner,
            CodexVendorOwnerRegistration::InactiveRoot => {
                return Err(EngineError::Conflict(
                    "Codex MCP request belongs to an inactive root turn".into(),
                ));
            }
            CodexVendorOwnerRegistration::Pending { id, receiver } => {
                let outcome = tokio::select! {
                    biased;
                    _ = root_cancel.cancelled() => {
                        self.bridged_tool_owners.abandon_vendor_owner(
                            root_thread_id,
                            vendor_thread_id,
                            id,
                        );
                        return Err(EngineError::Conflict("tool call cancelled".into()));
                    }
                    owner = receiver => owner.ok(),
                    _ = tokio::time::sleep(CODEX_BRIDGE_METADATA_WAIT_TIMEOUT) => None,
                };
                match outcome {
                    Some(owner) => owner,
                    None => {
                        self.bridged_tool_owners.abandon_vendor_owner(
                            root_thread_id,
                            vendor_thread_id,
                            id,
                        );
                        return Err(EngineError::BadRequest(format!(
                            "Codex MCP _meta.threadId {vendor_thread_id} is unknown, external to this root, or stale"
                        )));
                    }
                }
            }
        };
        if let Some(call_id) = vendor_call_id.filter(|call_id| !call_id.is_empty()) {
            let validation = self.bridged_tool_owners.register_call_validation(
                root_thread_id,
                vendor_thread_id,
                &owner_thread_id,
                call_id,
            );
            match validation {
                CodexCallValidationRegistration::Immediate => {}
                CodexCallValidationRegistration::Pending { id, receiver } => {
                    let outcome = tokio::select! {
                        biased;
                        _ = root_cancel.cancelled() => {
                            self.bridged_tool_owners.abandon_call_validation(
                                root_thread_id,
                                call_id,
                                id,
                            );
                            return Err(EngineError::Conflict("tool call cancelled".into()));
                        }
                        outcome = receiver => outcome.ok(),
                        _ = tokio::time::sleep(CODEX_BRIDGE_METADATA_WAIT_TIMEOUT) => None,
                    };
                    match outcome {
                        Some(CodexCallValidationOutcome::Matched) => {}
                        Some(CodexCallValidationOutcome::Mismatched) => {
                            return Err(EngineError::BadRequest(format!(
                                "Codex MCP _meta.callId {call_id} belongs to another vendor thread"
                            )));
                        }
                        None => {
                            self.bridged_tool_owners.abandon_call_validation(
                                root_thread_id,
                                call_id,
                                id,
                            );
                            return Err(EngineError::BadRequest(format!(
                                "Codex MCP _meta.callId {call_id} was not observed on the app-server stream"
                            )));
                        }
                    }
                }
                CodexCallValidationRegistration::InactiveRoot => {
                    return Err(EngineError::Conflict(
                        "Codex MCP request belongs to an inactive root turn".into(),
                    ));
                }
                CodexCallValidationRegistration::UnknownOwner => {
                    return Err(EngineError::BadRequest(format!(
                        "Codex MCP _meta.threadId {vendor_thread_id} is not bound to {owner_thread_id}"
                    )));
                }
                CodexCallValidationRegistration::MismatchedOwner => {
                    return Err(EngineError::BadRequest(format!(
                        "Codex MCP _meta.callId {call_id} belongs to another vendor thread"
                    )));
                }
                CodexCallValidationRegistration::Replayed => {
                    return Err(EngineError::Conflict(format!(
                        "Codex MCP _meta.callId {call_id} was already used or retired"
                    )));
                }
            }
        }
        if root_cancel.is_cancelled() {
            return Err(EngineError::Conflict("tool call cancelled".into()));
        }
        self.bridged_tool_call_for(&owner_thread_id, name, arguments, root_cancel)
            .await
    }

    async fn bridged_tool_call_for(
        self: &Arc<Self>,
        thread_id: &str,
        name: &str,
        arguments: &serde_json::Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, EngineError> {
        let (session, thread, mode, ctx) =
            self.bridged_context_with_cancel(thread_id, cancel.clone())?;
        let turn = self.store.last_turn(thread_id)?;
        let call = trouve_providers::ToolCallRequest {
            id: new_id("call"),
            name: name.to_string(),
            arguments: arguments.clone(),
        };
        // Bridged responses are text-only (MCP content blocks could carry
        // images, but no bridged vendor consumes them yet); the summary the
        // engine leaves in place of "_images" still tells the model the
        // image was read.
        let (content, _images) = self
            .handle_tool_call(&session, &thread, turn, &mode, &ctx, &call, &cancel)
            .await
            .map_err(EngineError::Internal)?;
        Ok(content)
    }

    fn announce_trouve_bridge_wrapper(
        &self,
        root_thread_id: &str,
        vendor_thread_id: &str,
        owner_thread_id: &str,
        call_id: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> bool {
        if trouve_bridge_wrapper_call(tool, args).is_none() {
            return false;
        }
        if !self.bridged_tool_owners.announce_wrapper(
            root_thread_id,
            vendor_thread_id,
            owner_thread_id,
            call_id,
        ) {
            tracing::warn!(
                root_thread_id,
                vendor_thread_id,
                owner_thread_id,
                call_id,
                "ignoring unbound, stale, or replayed Codex MCP wrapper identity"
            );
        }
        true
    }

    /// Consume Codex's MCP presentation lifecycle. The matching ToolExecutor
    /// call writes the sole durable card in the collaborator thread.
    fn suppress_collaborator_bridge_wrapper(
        &self,
        root_thread_id: &str,
        vendor_thread_id: &str,
        collaborator: &mut BackendCollaboratorProjection,
        event: &BackendCollaboratorEvent,
    ) -> bool {
        match event {
            BackendCollaboratorEvent::ToolStarted {
                call_id,
                tool,
                args,
            } if trouve_bridge_wrapper_call(tool, args).is_some() => {
                if collaborator.suppressed_bridge_calls.insert(call_id.clone()) {
                    self.announce_trouve_bridge_wrapper(
                        root_thread_id,
                        vendor_thread_id,
                        &collaborator.thread.id,
                        call_id,
                        tool,
                        args,
                    );
                }
                true
            }
            BackendCollaboratorEvent::ToolOutput { call_id, .. } => {
                collaborator.suppressed_bridge_calls.contains(call_id)
            }
            BackendCollaboratorEvent::ToolCompleted { call_id, .. } => {
                collaborator.suppressed_bridge_calls.remove(call_id)
            }
            _ => false,
        }
    }

    /// Gate a vendor-side tool call (Claude Code's `--permission-prompt-tool`
    /// hook) through trouve's permission layer. The vendor executes the tool
    /// itself if allowed; we only decide and record the decision. The
    /// approval attaches to the tool card the vendor's stream already
    /// created (the `tool_use` block precedes the permission request); a
    /// synthetic card is the fallback when no open call matches.
    pub async fn bridged_approval(
        &self,
        thread_id: &str,
        tool: &str,
        args: &serde_json::Value,
    ) -> Result<bool, EngineError> {
        let (session, thread, mode, ctx) = self.bridged_context(thread_id)?;
        let turn = self
            .store
            .last_turn(thread_id)
            .map_err(EngineError::Internal)?;
        let scope = Scope::Thread(thread.id.clone());
        let matched = self.open_vendor_call(&thread.id, turn, tool, args);
        let synthetic = matched.is_none();
        let call_id = matched.unwrap_or_else(|| new_id("appr"));
        // `gate_backend_approval` creates a missing tool card and batches it
        // with approval.requested. Do not pre-write a synthetic card here:
        // that used to force an extra SQLite transaction for every bridged
        // approval.
        let approved = self
            .gate_backend_approval(
                &session,
                &thread,
                turn,
                mode.read_only,
                &call_id,
                tool,
                args,
                &ctx.cancel,
            )
            .await
            .map_err(EngineError::Internal)?;
        // A matched card gets its completion from the vendor's own
        // tool_result; only the synthetic card needs closing here.
        if synthetic {
            self.store
                .append_event_async(
                    scope,
                    Event::ToolCompleted {
                        call_id,
                        status: if approved {
                            ToolStatus::Ok
                        } else {
                            ToolStatus::Denied
                        },
                        result: serde_json::json!(if approved { "approved" } else { "denied" }),
                        execution_duration_ms: None,
                    },
                )
                .await
                .map_err(EngineError::Internal)?;
        }
        Ok(approved)
    }

    /// The newest still-open vendor tool call in this turn that a
    /// permission request refers to: same tool, preferring an exact args
    /// match, never one already carrying an approval.
    fn open_vendor_call(
        &self,
        thread_id: &str,
        turn: u64,
        tool: &str,
        args: &serde_json::Value,
    ) -> Option<String> {
        let events = self
            .store
            .events_after(&Scope::Thread(thread_id.to_string()), 0)
            .ok()?;
        let mut open: Vec<(String, serde_json::Value)> = Vec::new();
        let mut gated: std::collections::HashSet<String> = Default::default();
        for env in &events {
            match &env.event {
                Event::ToolRequested {
                    turn: t,
                    call_id,
                    tool: name,
                    args: a,
                    ..
                } if *t == turn && name == tool => {
                    open.push((call_id.clone(), a.clone()));
                }
                Event::ToolCompleted { call_id, .. } => {
                    open.retain(|(id, _)| id != call_id);
                }
                Event::ApprovalRequested { call_id, .. } => {
                    gated.insert(call_id.clone());
                }
                _ => {}
            }
        }
        open.retain(|(id, _)| !gated.contains(id));
        // Stored args may carry injected "_line" display hints the vendor's
        // approval request doesn't have; ignore them when matching.
        let strip = |v: &serde_json::Value| {
            let mut v = v.clone();
            if let Some(map) = v.as_object_mut() {
                map.remove("_line");
                if let Some(edits) = map.get_mut("edits").and_then(|e| e.as_array_mut()) {
                    for e in edits {
                        if let Some(m) = e.as_object_mut() {
                            m.remove("_line");
                        }
                    }
                }
            }
            v
        };
        open.iter()
            .rev()
            .find(|(_, a)| strip(a) == *args)
            .or(open.last())
            .map(|(id, _)| id.clone())
    }

    /// Whether a `tool.requested` card already exists for this call in the
    /// current turn.
    fn tool_card_exists(&self, thread_id: &str, turn: u64, call_id: &str) -> bool {
        self.store
            .events_after(&Scope::Thread(thread_id.to_string()), 0)
            .ok()
            .is_some_and(|events| {
                events.iter().any(|env| {
                    matches!(
                        &env.event,
                        Event::ToolRequested {
                            turn: t,
                            call_id: id,
                            ..
                        } if *t == turn && id == call_id
                    )
                })
            })
    }

    /// Normalize a todo list from trouve's canonical shape or a supported
    /// vendor-native tool shape.
    fn parse_todo_snapshot(value: &serde_json::Value) -> Option<Vec<trouve_protocol::TodoItem>> {
        if let Ok(todos) = serde_json::from_value::<Vec<trouve_protocol::TodoItem>>(value.clone()) {
            return Some(todos);
        }

        // Claude's built-in TodoWrite omits ids and adds `activeForm`.
        // Normalize that vendor shape at the core boundary while keeping the
        // protocol's canonical TodoItem strict.
        value
            .as_array()?
            .iter()
            .map(|item| {
                let content = item.get("content")?.as_str()?.to_string();
                let status = serde_json::from_value(item.get("status")?.clone()).ok()?;
                let id = item
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("vendor:{content}"));
                Some(trouve_protocol::TodoItem {
                    id,
                    content,
                    status,
                })
            })
            .collect()
    }

    /// Persist a successful todo tool's authoritative result snapshot, or
    /// its paired start arguments when a vendor only returns an acknowledgement.
    fn persist_todos_from_result(
        &self,
        thread_id: &str,
        tool: &str,
        status: ToolStatus,
        result: &serde_json::Value,
        args: Option<&serde_json::Value>,
    ) -> Result<Option<Vec<trouve_protocol::TodoItem>>> {
        // TODO state is an authoritative first-party thread snapshot. Do not
        // infer ownership from a tool's basename: an unrelated MCP server can
        // legitimately expose its own `todo_write`, but its result must remain
        // an ordinary tool result rather than mutating trouve's thread state.
        let is_authoritative_todo_tool =
            matches!(tool, "todo_write" | "TodoWrite" | "mcp__trouve__todo_write");
        if status != ToolStatus::Ok || !is_authoritative_todo_tool {
            return Ok(None);
        }
        let result_todos = result.get("todos").and_then(Self::parse_todo_snapshot);
        let (mut todos, merge) = match result_todos {
            // Native trouve tools return the authoritative full snapshot,
            // including after a merge update.
            Some(todos) => (todos, false),
            // Vendor-native TodoWrite tools commonly return only an
            // acknowledgement. Their started event still carries the
            // requested list, so use that as the snapshot fallback.
            None => {
                let Some(args) = args else {
                    return Ok(None);
                };
                let Some(todos) = args.get("todos").and_then(Self::parse_todo_snapshot) else {
                    return Ok(None);
                };
                (
                    todos,
                    args.get("merge")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                )
            }
        };
        if merge {
            let mut merged = self
                .store
                .thread(thread_id)?
                .map(|thread| thread.todos)
                .unwrap_or_default();
            for todo in todos {
                match merged.iter_mut().find(|existing| existing.id == todo.id) {
                    Some(existing) => *existing = todo,
                    None => merged.push(todo),
                }
            }
            todos = merged;
        }
        self.store.update_thread_todos(thread_id, &todos)?;
        Ok(Some(todos))
    }

    fn bridged_context(
        &self,
        thread_id: &str,
    ) -> Result<(Session, Thread, AgentPersona, ToolCtx), EngineError> {
        let cancel = self.active_bridge_cancel(thread_id)?;
        self.bridged_context_with_cancel(thread_id, cancel)
    }

    fn active_bridge_cancel(
        &self,
        thread_id: &str,
    ) -> Result<tokio_util::sync::CancellationToken, EngineError> {
        let cancel = self
            .turn_cancels
            .lock()
            .unwrap()
            .get(thread_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::Conflict(format!(
                    "the tool bridge for thread {thread_id} is not attached to an active turn"
                ))
            })?;
        if cancel.is_cancelled() {
            return Err(EngineError::Conflict(format!(
                "the active turn for thread {thread_id} is cancelled"
            )));
        }
        Ok(cancel)
    }

    fn bridged_context_with_cancel(
        &self,
        thread_id: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(Session, Thread, AgentPersona, ToolCtx), EngineError> {
        let thread = self.get_thread(thread_id)?;
        let session = self.get_session(&thread.session_id)?;
        let ws = self
            .store
            .workspace(&session.workspace_id)
            .map_err(EngineError::Internal)?
            .ok_or_else(|| EngineError::NotFound("workspace".into()))?;
        let all_modes = self.resolve_personas(Some(Path::new(&ws.path)))?;
        let mut mode = personas::find_persona(&all_modes, &thread.mode)
            .cloned()
            .unwrap_or_else(personas::fallback_persona);
        if self.store.is_code_review_thread(&thread.id)? {
            mode = personas::secure_automated_review_persona(mode);
        }
        let worktree = PathBuf::from(&session.worktree_path);
        let canonical_worktree = worktree
            .canonicalize()
            .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        let ctx = ToolCtx {
            cancel,
            worktree,
            canonical_worktree: Some(canonical_worktree),
            read_only_roots: crate::skills::trusted_read_roots(
                self.config_dir.as_deref(),
                Some(Path::new(&ws.path)),
            )
            .into(),
            thread_id: thread.id.clone(),
            todos: Arc::new(Mutex::new(thread.todos.clone())),
            config_dir: self.config_dir.clone(),
            workspace_root: Some(PathBuf::from(&ws.path)),
            edit_strategy: edit_strategy_for_model(&thread.model),
            background_mutation_lease: None,
        };
        Ok((session, thread, mode, ctx))
    }

    /// User-configured MCP servers for a session's worktree, flattened for
    /// a vendor agent CLI: scopes merged (user < workspace < worktree),
    /// disabled entries dropped, env `${VAR}` references expanded. The name
    /// "trouve" is reserved for the bridge and skipped.
    fn mcp_servers_for(
        &self,
        session: &Session,
    ) -> Result<Vec<trouve_agents::McpServerLaunch>, EngineError> {
        let workspace_root = self
            .store
            .workspace(&session.workspace_id)?
            .map(|ws| PathBuf::from(ws.path));
        // Only trusted (user-config) servers are handed to the vendor CLI:
        // it would otherwise spawn a cloned repo's command with the expanded
        // environment, same RCE/exfiltration risk as the native path.
        let configs = crate::mcp::trusted_configs(
            self.config_dir.as_deref(),
            workspace_root.as_deref(),
            Path::new(&session.worktree_path),
        );
        Ok(configs
            .into_iter()
            .filter(|(name, _)| name != "trouve")
            .map(|(name, config)| trouve_agents::McpServerLaunch {
                name,
                command: config.command,
                args: config.args,
                env: config
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), crate::mcp::expand_env(v)))
                    .collect(),
            })
            .collect())
    }

    fn record_backend_collaborator_input(
        &self,
        collaborator: &mut BackendCollaboratorProjection,
        content: String,
    ) -> Result<()> {
        if content.is_empty() || collaborator.last_user_message.as_deref() == Some(&content) {
            return Ok(());
        }
        if !collaborator.segment.is_empty() {
            collaborator.persisted.push(Event::AssistantMessage {
                turn: collaborator.turn,
                content: std::mem::take(&mut collaborator.segment),
            });
        }
        let event = if collaborator.last_user_message.is_some() {
            Event::TurnSteered {
                turn: collaborator.turn,
                content: content.clone(),
                attachments: Vec::new(),
            }
        } else {
            Event::UserMessage {
                turn: collaborator.turn,
                content: content.clone(),
                attachments: Vec::new(),
                background: false,
            }
        };
        collaborator.persisted.push(event);
        self.store.append_message(
            &collaborator.thread.id,
            &serde_json::to_value(Message::User(content.clone()))?,
        )?;
        collaborator.last_user_message = Some(content);
        Ok(())
    }

    async fn begin_backend_collaborator_turn(
        &self,
        collaborator: &mut BackendCollaboratorProjection,
        vendor_turn_id: Option<String>,
        prompt: String,
    ) -> Result<()> {
        debug_assert!(collaborator.persisted.is_empty());
        collaborator.turn = self.store.next_turn(&collaborator.thread.id)?;
        collaborator.vendor_turn_id = vendor_turn_id;
        collaborator.last_user_message = None;
        collaborator.pending_prompt = None;
        collaborator.text.clear();
        collaborator.segment.clear();
        collaborator.usage = Usage::default();
        collaborator.tool_calls.clear();
        collaborator.tool_started_at.clear();
        collaborator.suppressed_bridge_calls.clear();
        collaborator.terminal = false;
        // Provider-native collaborators execute inside the containing vendor
        // turn and therefore inherit its already-acquired capacity. Publish
        // that fact before `TurnStarted` so direct and nested children project
        // as running instead of remaining in the scheduler's waiting state.
        self.store
            .append_events_async(
                Scope::Thread(collaborator.thread.id.clone()),
                vec![
                    Event::TurnCapacityAcquired {
                        turn: collaborator.turn,
                        wait_ms: 0,
                        background: false,
                    },
                    Event::TurnStarted {
                        turn: collaborator.turn,
                        mode: collaborator.thread.mode.clone(),
                        model: collaborator.thread.model.clone(),
                        thinking_level: collaborator.thinking_level.clone(),
                        // The containing app-server turn owns native child steering.
                        supports_steering: false,
                    },
                ],
            )
            .await?;
        self.record_backend_collaborator_input(collaborator, prompt)
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_backend_collaborator_claimed(
        &self,
        session: &Session,
        parent_thread: &Thread,
        backend_id: &str,
        vendor_session_id: String,
        parent_vendor_session_id: &str,
        name: Option<String>,
        access: BackendCollaboratorAccess,
        prompt: Option<String>,
        model: Option<String>,
        thinking_level: Option<String>,
        collaborator_claims: &mut BackendCollaboratorClaims<'_>,
        vendor_threads: &mut HashMap<String, String>,
        collaborators: &mut HashMap<String, BackendCollaboratorProjection>,
    ) -> Result<()> {
        let prompt = prompt.filter(|prompt| !prompt.is_empty());
        if let Some(collaborator) = collaborators.get_mut(&vendor_session_id) {
            if !collaborator_claims.claim(&collaborator.thread.id, &session.id) {
                bail!(
                    "cannot reactivate provider collaborator {} while another turn owns it",
                    collaborator.thread.id
                );
            }
            if let Some(prompt) = prompt {
                if collaborator.terminal {
                    collaborator.pending_prompt = Some(prompt);
                } else {
                    self.record_backend_collaborator_input(collaborator, prompt)?;
                }
            }
            return Ok(());
        }
        if let Some(existing_thread_id) = vendor_threads.get(&vendor_session_id) {
            // A provider-native activity notification can point from a child
            // back to its parent/root. That vendor session is already bound;
            // materializing it again would create a phantom descendant whose
            // turn can never finish and would keep the real root active.
            tracing::warn!(
                vendor_session_id,
                existing_thread_id,
                parent_vendor_session_id,
                "ignoring collaborator announcement for an already-bound vendor session"
            );
            return Ok(());
        }
        let parent_thread_id = vendor_threads
            .get(parent_vendor_session_id)
            .cloned()
            .unwrap_or_else(|| parent_thread.id.clone());
        let inherited_thread = collaborators
            .values()
            .find(|collaborator| collaborator.thread.id == parent_thread_id)
            .map(|collaborator| collaborator.thread.clone())
            .unwrap_or_else(|| parent_thread.clone());
        let (root_thread_id, depth) = self
            .subagent_root_and_depth(&inherited_thread.id)
            .map_err(|error| anyhow!(error.to_string()))?;
        if depth >= MAX_SUBAGENT_DEPTH {
            bail!(
                "provider collaborator nesting is limited to {MAX_SUBAGENT_DEPTH} levels below the root thread"
            );
        }
        let tree_lock = self.subagent_tree_lock(&root_thread_id);
        let _tree_spawn = tree_lock.lock().await;
        let workspace = self
            .store
            .workspace(&session.workspace_id)?
            .ok_or_else(|| anyhow!("workspace {} not found", session.workspace_id))?;
        let all_modes = self.resolve_personas(Some(Path::new(&workspace.path)))?;
        let inherited_mode = personas::find_persona(&all_modes, &inherited_thread.mode)
            .cloned()
            .unwrap_or_else(personas::fallback_persona);
        if !inherited_mode.allowed_tools.is_empty()
            && !inherited_mode
                .allowed_tools
                .iter()
                .any(|tool| tool == "spawn_thread")
        {
            bail!(
                "provider-native collaborators are not permitted in {} mode",
                inherited_mode.id
            );
        }
        let children = self.store.spawned_children(&inherited_thread.id)?;
        let descendants = self
            .list_thread_descendants(&root_thread_id)
            .map_err(|error| anyhow!(error.to_string()))?;
        {
            let active = self.active_threads.lock().unwrap();
            let running = children
                .iter()
                .filter(|child| active.contains_key(*child))
                .count();
            if running >= MAX_CONCURRENT_CHILDREN {
                bail!(
                    "provider collaborator parent already has {running} active children; the limit is {MAX_CONCURRENT_CHILDREN}"
                );
            }
            let active_descendants = descendants
                .iter()
                .filter(|descendant| active.contains_key(&descendant.id))
                .count();
            if active_descendants >= MAX_ACTIVE_DESCENDANTS {
                bail!(
                    "provider collaborator tree already has {active_descendants} active descendants; the limit is {MAX_ACTIVE_DESCENDANTS}"
                );
            }
        }
        let model = model
            .filter(|model| !model.trim().is_empty())
            .map(|model| {
                if model.contains('/') {
                    model
                } else {
                    format!("{backend_id}/{model}")
                }
            })
            .unwrap_or_else(|| inherited_thread.model.clone());
        let mut model_options = if model == inherited_thread.model {
            self.store.thread_model_options(&inherited_thread.id)?
        } else {
            serde_json::Map::new()
        };
        let mut thinking_level = thinking_level.filter(|level| !level.trim().is_empty());
        if let Some(level) = thinking_level.as_ref() {
            // The canonical inherited key keeps this useful even when the
            // selected model advertises a differently named vendor option.
            model_options.insert("thinking_level".into(), serde_json::json!(level));
        }
        thinking_level = thinking_level.or_else(|| {
            THINKING_OPTION_KEYS.iter().find_map(|key| {
                model_options
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        });
        let title = self
            .generate_subagent_title(name.as_deref(), prompt.as_deref())
            .await;
        let child_mode = self.backend_collaborator_mode(session, &inherited_thread, access)?;
        let collaborator_mode = personas::find_persona(&all_modes, &child_mode)
            .cloned()
            .unwrap_or_else(personas::fallback_persona);
        let child = collaborator_claims
            .create_claimed_thread(&session.id, || {
                self.create_spawned_thread_for_session(
                    session.clone(),
                    CreateThreadRequest {
                        session_id: session.id.clone(),
                        title,
                        mode: Some(child_mode),
                        model: Some(model),
                        model_options,
                        permission_mode: Some(inherited_thread.permission_mode),
                    },
                    &parent_thread_id,
                    "vendor",
                )
            })
            .map_err(|error| anyhow!(error.to_string()))?;
        self.store
            .set_backend_session(&child.id, backend_id, &vendor_session_id)?;
        vendor_threads.insert(vendor_session_id.clone(), child.id.clone());
        let mut collaborator = BackendCollaboratorProjection {
            thread: child,
            mode: collaborator_mode,
            turn: 0,
            spawn_link_published: false,
            vendor_turn_id: None,
            thinking_level,
            last_user_message: None,
            pending_prompt: None,
            text: String::new(),
            segment: String::new(),
            usage: Usage::default(),
            tool_calls: HashMap::new(),
            tool_started_at: HashMap::new(),
            suppressed_bridge_calls: HashSet::new(),
            mutation_permits: HashMap::new(),
            pending_approval: None,
            approval_cancels: HashMap::new(),
            persisted: Vec::new(),
            terminal: true,
        };
        self.begin_backend_collaborator_turn(&mut collaborator, None, prompt.unwrap_or_default())
            .await?;
        collaborators.insert(vendor_session_id, collaborator);
        Ok(())
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    async fn start_backend_collaborator(
        &self,
        session: &Session,
        parent_thread: &Thread,
        backend_id: &str,
        vendor_session_id: String,
        parent_vendor_session_id: &str,
        name: Option<String>,
        access: BackendCollaboratorAccess,
        prompt: Option<String>,
        model: Option<String>,
        thinking_level: Option<String>,
        vendor_threads: &mut HashMap<String, String>,
        collaborators: &mut HashMap<String, BackendCollaboratorProjection>,
    ) -> Result<()> {
        let mut claims = BackendCollaboratorClaims::new(&self.active_threads);
        self.start_backend_collaborator_claimed(
            session,
            parent_thread,
            backend_id,
            vendor_session_id,
            parent_vendor_session_id,
            name,
            access,
            prompt,
            model,
            thinking_level,
            &mut claims,
            vendor_threads,
            collaborators,
        )
        .await
    }

    async fn publish_backend_collaborator_spawn(
        &self,
        root_thread: &Thread,
        root_turn: u64,
        vendor_session_id: &str,
        collaborators: &mut HashMap<String, BackendCollaboratorProjection>,
    ) -> Result<()> {
        let Some(collaborator) = collaborators
            .get(vendor_session_id)
            .filter(|collaborator| !collaborator.spawn_link_published)
        else {
            return Ok(());
        };
        let Some(child_prompt) = collaborator
            .last_user_message
            .clone()
            .filter(|prompt| !prompt.is_empty())
        else {
            // Some providers announce collaborator activity before an
            // asynchronous prompt lookup completes. Delay the one durable
            // parent link until that lookup supplies a real prompt; the event
            // log is append-only, so an empty event cannot be updated later.
            return Ok(());
        };
        let child_thread_id = collaborator.thread.id.clone();
        let child_session_id = collaborator.thread.session_id.clone();
        let child_model = collaborator.thread.model.clone();
        let parent_thread_id = self
            .store
            .spawn_parent(&child_thread_id)?
            .unwrap_or_else(|| root_thread.id.clone());
        let parent_turn = if parent_thread_id == root_thread.id {
            root_turn
        } else {
            collaborators
                .values()
                .find(|candidate| candidate.thread.id == parent_thread_id)
                .map_or(root_turn, |candidate| candidate.turn)
        };
        self.store
            .append_event_async(
                Scope::Thread(parent_thread_id),
                Event::SubagentSpawned {
                    turn: parent_turn,
                    thread_id: child_thread_id,
                    session_id: child_session_id,
                    prompt: child_prompt,
                    model: child_model,
                    call_id: None,
                },
            )
            .await?;
        if let Some(collaborator) = collaborators.get_mut(vendor_session_id) {
            collaborator.spawn_link_published = true;
        }
        Ok(())
    }

    async fn finish_backend_collaborator(
        &self,
        session: &Session,
        backend_id: &str,
        collaborator: &mut BackendCollaboratorProjection,
        outcome: Result<Usage, String>,
    ) -> Result<()> {
        if collaborator.terminal {
            return Ok(());
        }
        // A terminal collaborator cannot emit a later completion event; drop
        // every vendor-native mutation lease before persistence bookkeeping.
        collaborator.mutation_permits.clear();
        collaborator.pending_approval = None;
        for cancel in collaborator
            .approval_cancels
            .drain()
            .map(|(_, cancel)| cancel)
        {
            cancel.cancel();
        }
        if !collaborator.segment.is_empty() {
            collaborator.persisted.push(Event::AssistantMessage {
                turn: collaborator.turn,
                content: std::mem::take(&mut collaborator.segment),
            });
        }
        flush_backend_event_batch(
            &self.store,
            &Scope::Thread(collaborator.thread.id.clone()),
            &mut collaborator.persisted,
        )
        .await?;
        self.store.append_message(
            &collaborator.thread.id,
            &serde_json::to_value(Message::Assistant {
                content: collaborator.text.clone(),
                tool_calls: Vec::new(),
                reasoning: Vec::new(),
            })?,
        )?;
        match outcome {
            Ok(mut usage) => {
                let context_input_tokens = usage.context_input_tokens.unwrap_or_else(|| {
                    usage.input_tokens.saturating_add(usage.cached_input_tokens)
                });
                usage.context_input_tokens = Some(context_input_tokens);
                collaborator.usage = usage.clone();
                let seen = self.store.messages(&collaborator.thread.id)?.len() as u64;
                self.store
                    .mark_backend_seen(&collaborator.thread.id, backend_id, seen)?;
                self.store.record_usage(
                    &session.id,
                    &collaborator.thread.id,
                    collaborator.turn,
                    &collaborator.thread.model,
                    &usage,
                    context_input_tokens,
                )?;
                self.store.append_event(
                    Scope::Thread(collaborator.thread.id.clone()),
                    Event::TurnCompleted {
                        turn: collaborator.turn,
                        usage,
                        checkpoint_id: None,
                    },
                )?;
            }
            Err(error) if error == "turn cancelled" => {
                self.store.append_event(
                    Scope::Thread(collaborator.thread.id.clone()),
                    Event::TurnCancelled {
                        turn: collaborator.turn,
                    },
                )?;
            }
            Err(error) => {
                self.store.append_event(
                    Scope::Thread(collaborator.thread.id.clone()),
                    Event::TurnFailed {
                        turn: collaborator.turn,
                        error,
                    },
                )?;
            }
        }
        collaborator.terminal = true;
        Ok(())
    }

    async fn prepare_backend_collaborator_turn(
        &self,
        session: &Session,
        backend_id: &str,
        collaborator: &mut BackendCollaboratorProjection,
        vendor_turn_id: Option<&str>,
    ) -> Result<()> {
        let Some(vendor_turn_id) = vendor_turn_id else {
            return Ok(());
        };
        match collaborator.vendor_turn_id.as_deref() {
            Some(current) if current == vendor_turn_id => return Ok(()),
            None if !collaborator.terminal => {
                collaborator.vendor_turn_id = Some(vendor_turn_id.to_string());
                return Ok(());
            }
            _ => {}
        }
        if !collaborator.terminal {
            self.finish_backend_collaborator(
                session,
                backend_id,
                collaborator,
                Err("vendor collaborator started a replacement turn before completing the prior turn".into()),
            )
            .await?;
        }
        let prompt = collaborator.pending_prompt.take().unwrap_or_default();
        self.begin_backend_collaborator_turn(collaborator, Some(vendor_turn_id.to_string()), prompt)
            .await
    }

    async fn persist_backend_collaborator_event(
        &self,
        session: &Session,
        _root_mode: &AgentPersona,
        backend_id: &str,
        collaborator: &mut BackendCollaboratorProjection,
        event: BackendCollaboratorEvent,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        if collaborator.terminal {
            // Codex may have to recover a collaborator's initial prompt with
            // an asynchronous thread/turns/list request. A short-lived child
            // can finish before that lookup returns; dropping the late user
            // event leaves the durable transcript with Thought as its first
            // chat node. Preserve the one missing initial prompt as a display
            // event even after the vendor turn is terminal. Do not append it
            // to the provider transcript: finish_backend_collaborator already
            // stored the completed Assistant message, so doing so would
            // reverse their causal order for a direct continuation. The
            // vendor session itself already contains the original prompt.
            if let BackendCollaboratorEvent::UserMessage(content) = event
                && !content.is_empty()
                && collaborator.last_user_message.is_none()
            {
                collaborator.persisted.push(Event::UserMessage {
                    turn: collaborator.turn,
                    content: content.clone(),
                    attachments: Vec::new(),
                    background: false,
                });
                collaborator.last_user_message = Some(content);
                flush_backend_event_batch(
                    &self.store,
                    &Scope::Thread(collaborator.thread.id.clone()),
                    &mut collaborator.persisted,
                )
                .await?;
            }
            return Ok(());
        }
        let turn = collaborator.turn;
        match event {
            BackendCollaboratorEvent::TurnStarted => {}
            BackendCollaboratorEvent::UserMessage(content) => {
                self.record_backend_collaborator_input(collaborator, content)?;
            }
            BackendCollaboratorEvent::TextDelta(delta) => {
                collaborator.text.push_str(&delta);
                collaborator.segment.push_str(&delta);
                collaborator
                    .persisted
                    .push(Event::AssistantDelta { turn, text: delta });
            }
            BackendCollaboratorEvent::ProgressDelta(delta) => {
                if !collaborator.segment.is_empty() {
                    collaborator.persisted.push(Event::AssistantMessage {
                        turn,
                        content: std::mem::take(&mut collaborator.segment),
                    });
                }
                collaborator
                    .persisted
                    .push(Event::AssistantProgress { turn, text: delta });
            }
            BackendCollaboratorEvent::ProgressCompleted => collaborator
                .persisted
                .push(Event::AssistantProgressCompleted { turn }),
            BackendCollaboratorEvent::ThinkingDelta(delta) => {
                if !collaborator.segment.is_empty() {
                    collaborator.persisted.push(Event::AssistantMessage {
                        turn,
                        content: std::mem::take(&mut collaborator.segment),
                    });
                }
                collaborator
                    .persisted
                    .push(Event::AssistantThinking { turn, text: delta });
            }
            BackendCollaboratorEvent::ThinkingCompleted => collaborator
                .persisted
                .push(Event::AssistantThinkingCompleted { turn }),
            BackendCollaboratorEvent::ToolStarted {
                call_id,
                tool,
                mut args,
            } => {
                collaborator
                    .tool_started_at
                    .insert(call_id.clone(), Instant::now());
                collaborator
                    .tool_calls
                    .insert(call_id.clone(), (tool.clone(), args.clone()));
                if !collaborator.segment.is_empty() {
                    collaborator.persisted.push(Event::AssistantMessage {
                        turn,
                        content: std::mem::take(&mut collaborator.segment),
                    });
                }
                annotate_edit_lines(Path::new(&session.worktree_path), &mut args);
                if !self.tool_card_exists(&collaborator.thread.id, turn, &call_id) {
                    collaborator.persisted.push(Event::ToolRequested {
                        turn,
                        call_id: call_id.clone(),
                        tool,
                        args,
                        requires_approval: false,
                    });
                }
                collaborator.persisted.push(Event::ToolStarted { call_id });
            }
            BackendCollaboratorEvent::ToolOutput { call_id, chunk } => collaborator
                .persisted
                .push(Event::ToolOutput { call_id, chunk }),
            BackendCollaboratorEvent::ToolCompleted {
                call_id,
                ok,
                result,
            } => {
                // Match the root backend path: the vendor has acknowledged
                // tool completion, so release its exclusive worktree lane
                // before event persistence and result-derived bookkeeping.
                collaborator.mutation_permits.remove(&call_id);
                flush_backend_event_batch(
                    &self.store,
                    &Scope::Thread(collaborator.thread.id.clone()),
                    &mut collaborator.persisted,
                )
                .await?;
                let status = if ok {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Error
                };
                let execution_duration_ms = collaborator
                    .tool_started_at
                    .remove(&call_id)
                    .map(monotonic_elapsed_ms);
                let todos = match collaborator.tool_calls.get(&call_id) {
                    Some((tool, args)) => self.persist_todos_from_result(
                        &collaborator.thread.id,
                        tool,
                        status,
                        &result,
                        Some(args),
                    )?,
                    None => None,
                };
                collaborator.persisted.push(Event::ToolCompleted {
                    call_id,
                    status,
                    result,
                    execution_duration_ms,
                });
                if let Some(todos) = todos {
                    collaborator.persisted.push(Event::TodosUpdated { todos });
                }
            }
            BackendCollaboratorEvent::ApprovalNeeded {
                call_id,
                tool,
                args,
                responder,
            } => {
                if !collaborator.segment.is_empty() {
                    collaborator.persisted.push(Event::AssistantMessage {
                        turn,
                        content: std::mem::take(&mut collaborator.segment),
                    });
                }
                flush_backend_event_batch(
                    &self.store,
                    &Scope::Thread(collaborator.thread.id.clone()),
                    &mut collaborator.persisted,
                )
                .await?;
                collaborator.pending_approval = Some(PendingCollaboratorApproval {
                    thread: collaborator.thread.clone(),
                    turn,
                    mode: collaborator.mode.clone(),
                    call_id,
                    tool,
                    args,
                    responder,
                });
            }
            BackendCollaboratorEvent::TodosUpdated { todos } => {
                flush_backend_event_batch(
                    &self.store,
                    &Scope::Thread(collaborator.thread.id.clone()),
                    &mut collaborator.persisted,
                )
                .await?;
                self.store
                    .update_thread_todos(&collaborator.thread.id, &todos)?;
                collaborator.persisted.push(Event::TodosUpdated { todos });
            }
            BackendCollaboratorEvent::UsageUpdated { usage } => {
                collaborator
                    .persisted
                    .push(Event::TurnUsageUpdated { turn, usage });
            }
            BackendCollaboratorEvent::CompactionStarted => {
                if !collaborator.segment.is_empty() {
                    collaborator.persisted.push(Event::AssistantMessage {
                        turn,
                        content: std::mem::take(&mut collaborator.segment),
                    });
                }
                collaborator
                    .persisted
                    .push(Event::CompactionStarted { turn });
            }
            BackendCollaboratorEvent::CompactionCompleted => {
                collaborator.persisted.push(Event::CompactionCompleted {
                    turn,
                    messages_compacted: 0,
                });
            }
            BackendCollaboratorEvent::CompactionFailed => {
                collaborator
                    .persisted
                    .push(Event::CompactionFailed { turn });
            }
            BackendCollaboratorEvent::Completed { usage } => {
                self.finish_backend_collaborator(session, backend_id, collaborator, Ok(usage))
                    .await?;
            }
            BackendCollaboratorEvent::Failed { error } => {
                self.finish_backend_collaborator(session, backend_id, collaborator, Err(error))
                    .await?;
            }
        }
        Ok(())
    }

    /// Run one turn through an external agent backend. The vendor harness
    /// plans, calls tools, and edits the worktree; we persist its events,
    /// gate its approval requests through our permission layer, and keep the
    /// checkpoint/usage flow identical to native turns. Compaction and the
    /// system prompt are the vendor's job (the mode prompt rides along as
    /// appended instructions); the local transcript is kept for rendering
    /// and history, not as the model's context.
    #[allow(clippy::too_many_arguments)]
    async fn run_backend_turn(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
        backend_id: &str,
        backend: Arc<dyn AgentBackend>,
        model_name: String,
        content: String,
        attachments: Vec<trouve_protocol::Attachment>,
        cancel: tokio_util::sync::CancellationToken,
        queued_prompt_id: &str,
        tools_enabled: bool,
        attach_background: bool,
    ) -> Result<()> {
        let startup_started = Instant::now();
        let scope = Scope::Thread(thread.id.clone());
        let mut model_options = self.store.thread_model_options(&thread.id)?;
        let model_catalog_started = Instant::now();
        let model_catalog = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            models = backend.list_models() => models,
        };
        tracing::info!(
            thread_id = %thread.id,
            turn,
            backend = %backend_id,
            elapsed_ms = model_catalog_started.elapsed().as_millis(),
            "agent startup timing: model catalog resolved"
        );
        let selected_model = model_catalog.iter().find(|m| m.id == thread.model);
        normalize_thinking_option(&mut model_options, selected_model);
        let supports_steering = tools_enabled && backend.supports_steering();
        // Some vendor protocols cannot remove their built-in read/search
        // tools. Keep those turns restricted (no mounted MCP tools and
        // read-only permission), but reserve strict tool-use rejection for
        // backends that can actually guarantee a tool-free surface.
        let strict_tool_free = !tools_enabled && backend.supports_tool_free_turns();
        // Vendor sessions are per (thread, backend): each vendor keeps its
        // own history, and switching models away and back resumes it.
        // Vendors can't read our transcript, so whatever part of the
        // thread's past this one hasn't seen — everything for a vendor
        // joining mid-conversation, the interleaved turns other models ran
        // for a resumed one — is handed off as a digest in the prompt.
        // A vendor session retains the tools it was created with. Restricted
        // repair turns therefore start fresh; their prompt carries the
        // malformed output explicitly, so they do not need vendor history.
        let (resume, handoff) = if tools_enabled {
            let resume = self.store.backend_session(&thread.id, backend_id)?;
            let payloads = self.store.messages(&thread.id)?;
            let unseen = match &resume {
                // A compaction can shrink the transcript below the watermark;
                // handing off the fresh summary again covers that.
                Some((_, seen)) => payloads.get(*seen as usize..).unwrap_or(&payloads),
                None => &payloads[..],
            };
            let messages: Vec<Message> = unseen
                .iter()
                .filter_map(|p| serde_json::from_value(p.clone()).ok())
                .collect();
            let handoff = render_history_digest(&messages, resume.is_some());
            (resume, handoff)
        } else {
            (None, None)
        };
        let vendor_session = resume.map(|(id, _)| id);
        let mut active_vendor_session = vendor_session.clone();
        if let Some(vendor_session_id) = active_vendor_session.as_deref() {
            self.bridged_tool_owners
                .bind_vendor_thread(&thread.id, vendor_session_id, &thread.id)
                .map_err(anyhow::Error::msg)?;
        }
        // Images go to the vendor protocol as native image inputs; other
        // files become path references in the prompt text (vendor agents
        // run on this filesystem and can read them with their tools).
        let materialized = self
            .materialize_attachments_for_turn(session, &attachments, &cancel)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let (images, files): (Vec<_>, Vec<_>) = materialized
            .into_iter()
            .partition(|file| file.attachment.mime.starts_with("image/"));
        let prompt_files = files
            .iter()
            .map(|file| (file.attachment.clone(), file.relative_path.clone()))
            .collect::<Vec<_>>();
        let content = annotate_attachments(content, &prompt_files);
        let turn_attachments: Vec<trouve_agents::TurnAttachment> = images
            .into_iter()
            .map(|file| trouve_agents::TurnAttachment {
                name: file.attachment.name,
                mime: file.attachment.mime,
                bytes: file.bytes,
                local_path: Some(file.absolute_path),
            })
            .collect();
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::User(content.clone()))?,
        )?;
        if !self.store.finish_queued_prompt(queued_prompt_id)? {
            bail!("queued prompt {queued_prompt_id} vanished before turn start");
        }

        let effective_read_only = !tools_enabled || mode.read_only;
        let permission = if effective_read_only {
            BackendPermission::ReadOnly
        } else {
            // Always request a pre-execution callback. Trouve's gate still
            // auto-approves Yolo calls, while the callback acquires the
            // session mutation lane and supplies creator provenance.
            BackendPermission::Ask
        };

        let mcp_bridge = tools_enabled
            .then(|| self.mcp_bridge_for(&thread.model, &thread.id))
            .flatten();
        // Vendor agents get the mode prompt plus, when the bridge serves
        // trouve's search tools, guidance to prefer them over built-ins
        // (MCP instructions alone are too weak a signal).
        let mut instructions = mode.system_prompt.trim().to_string();
        if mcp_bridge.is_some() {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(crate::tools::VENDOR_SEARCH_GUIDANCE);
        }
        let full_tool_bridge = mcp_bridge
            .as_ref()
            .is_some_and(|bridge| bridge.bridge_tools);
        let automated_review = self.store.is_code_review_thread(&thread.id)?;
        enforce_automated_review_backend_boundary(
            automated_review,
            tools_enabled,
            full_tool_bridge,
            backend.confines_read_only_turns(),
            backend_id,
        )?;
        if full_tool_bridge {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(crate::tools::VENDOR_TOOL_BRIDGE_GUIDANCE);
        }
        // A full bridge already exposes user MCP servers through the local
        // ToolExecutor. Mounting them directly as well would bypass trouve's
        // permission and per-session concurrency gates.
        let mcp_servers = if tools_enabled && !full_tool_bridge && !automated_review {
            self.mcp_servers_for(session)?
        } else {
            Vec::new()
        };
        // The digest decorates only the prompt sent to the vendor; the
        // stored transcript keeps the user's words alone.
        let prompt = match &handoff {
            Some(digest) => format!("{digest}\n\n{content}"),
            None => content,
        };
        let backend_turn = BackendTurn {
            cancel: cancel.clone(),
            thread_id: thread.id.clone(),
            worktree: PathBuf::from(&session.worktree_path),
            session: vendor_session,
            model: model_name,
            model_options,
            prompt,
            attachments: turn_attachments,
            instructions: (!instructions.is_empty()).then_some(instructions),
            permission,
            tool_free: strict_tool_free,
            attach_background,
            mcp_bridge,
            mcp_servers,
        };

        let startup_activity = backend.startup_activity(&backend_turn).await;
        if matches!(
            startup_activity,
            Some(BackendStartupActivity::ConnectingTools)
        ) {
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::TurnPhaseChanged {
                        turn,
                        phase: TurnPhase::ConnectingTools,
                    },
                )
                .await?;
        }
        let backend_turn_started = Instant::now();
        let mut stream = match backend.run_turn(backend_turn).await {
            Ok(stream) => stream,
            Err(BackendError::Cancelled) if cancel.is_cancelled() => return Ok(()),
            Err(error) => return Err(anyhow!("backend error: {error}")),
        };
        if startup_activity.is_some() {
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::TurnPhaseChanged {
                        turn,
                        phase: TurnPhase::Processing,
                    },
                )
                .await?;
        }
        tracing::info!(
            thread_id = %thread.id,
            turn,
            backend = %backend_id,
            elapsed_ms = backend_turn_started.elapsed().as_millis(),
            since_turn_started_ms = startup_started.elapsed().as_millis(),
            "agent startup timing: vendor turn accepted"
        );

        let mut steer_rx = None;
        let (steer_mutation_lane_state, _) =
            tokio::sync::watch::channel(SteerMutationLaneState::Idle);
        let _steerer_guard = if supports_steering {
            let (sender, receiver) = tokio::sync::mpsc::channel(8);
            let replaced = self.turn_steerers.lock().unwrap().insert(
                thread.id.clone(),
                ActiveTurnSteerer {
                    turn,
                    sender,
                    mutation_lane_state: steer_mutation_lane_state.clone(),
                },
            );
            if let Some(replaced) = replaced {
                replaced
                    .mutation_lane_state
                    .send_replace(SteerMutationLaneState::Ended);
            }
            steer_rx = Some(receiver);
            Some(ActiveTurnSteererGuard {
                registry: &self.turn_steerers,
                thread_id: thread.id.clone(),
                turn,
            })
        } else {
            None
        };

        // `text` records the whole turn for the transcript; `segment` is the
        // current streamed block, flushed (finalized) at each tool boundary
        // so tool cards interleave with the text in the order they happened
        // instead of all text merging into one leading bubble.
        let mut text = String::new();
        let mut segment = String::new();
        let mut usage_total = Usage::default();
        // Vendor-native todo tools are reported as ordinary tool events.
        // Remember their names until completion so their result can update
        // the same persisted snapshot as trouve's bridged/native tool.
        let mut tool_calls =
            HashMap::<String, (String, serde_json::Value, PullRequestCreationRequest)>::new();
        let mut tool_started_at = HashMap::<String, Instant>::new();
        // Creation tools sometimes stream their final PR URL before the
        // completion payload. Buffer output only for calls whose request is
        // demonstrably creating a PR; list/view output must never associate
        // every PR it happens to mention with this session.
        let mut github_creation_output = HashMap::<String, String>::new();
        // A vendor may use any GitHub client instead of trouve's create-PR
        // endpoint. Turn repository-specific PR references in its output into
        // the same durable session event, independent of the tool name.
        // Repository discovery shells out to Git. Defer it until a tool call
        // can plausibly create a pull request so ordinary turns do not pay
        // that process-startup cost.
        let mut github_repository: Option<(String, String, String)> = None;
        let mut vendor_threads = HashMap::<String, String>::new();
        if let Some(vendor_session_id) = active_vendor_session.as_ref() {
            vendor_threads.insert(vendor_session_id.clone(), thread.id.clone());
        }
        let mut collaborators = HashMap::<String, BackendCollaboratorProjection>::new();
        let mut collaborator_claims = BackendCollaboratorClaims::new(&self.active_threads);
        let mut persisted = Vec::new();
        let mut persist_deadline = None;
        let mut seen_tool_cards = HashSet::new();
        let mut suppressed_bridge_calls = HashSet::new();
        let mut pending_backend_approvals = futures::stream::FuturesUnordered::new();
        let mut backend_approval_cancels =
            HashMap::<String, tokio_util::sync::CancellationToken>::new();
        let mut backend_mutation_permits =
            HashMap::<String, tokio::sync::OwnedRwLockWriteGuard<()>>::new();
        let mut pending_steer = None;
        let mut pending_steer_lane = None;
        let mut pending_steer_permit = None;
        let mut consecutive_backend_events = 0usize;
        let mut first_substantive_event = true;
        loop {
            let flush_at = persist_deadline.unwrap_or_else(Instant::now);
            let steer_reserved = active_vendor_session.is_some()
                && !cancel.is_cancelled()
                && reserve_ready_steer_after_event_budget(
                    &mut steer_rx,
                    &mut pending_steer,
                    &mut consecutive_backend_events,
                );
            let input = if pending_steer_lane.is_some() {
                // Poll lane acquisition ahead of the backend stream so a
                // continuously-ready stream cannot starve it. If the lane is
                // still held, the future stays pending and ToolCompleted can
                // flow through the event branch to release it.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        reject_pending_steer(&mut pending_steer, "turn cancelled");
                        steer_mutation_lane_state
                            .send_replace(SteerMutationLaneState::Idle);
                        drop(pending_steer_lane.take());
                        drop(pending_steer_permit.take());
                        continue;
                    }
                    approval = pending_backend_approvals.next(), if !pending_backend_approvals.is_empty() => {
                        BackendLoopInput::Approval(
                            approval.expect("non-empty approval queue must yield an outcome")
                        )
                    }
                    permit = async {
                        pending_steer_lane
                            .as_mut()
                            .expect("guarded pending steering lane future")
                            .await
                    } => {
                        pending_steer_lane = None;
                        steer_mutation_lane_state
                            .send_replace(SteerMutationLaneState::Idle);
                        pending_steer_permit = Some(permit);
                        continue;
                    }
                    event = stream.next() => BackendLoopInput::Event(event),
                    _ = tokio::time::sleep_until(flush_at.into()), if persist_deadline.is_some() => {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                        persist_deadline = None;
                        continue;
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled(), if pending_steer.is_some() => {
                        reject_pending_steer(&mut pending_steer, "turn cancelled");
                        drop(pending_steer_permit.take());
                        continue;
                    }
                    // Persist vendor output that is already available before
                    // accepting simultaneously-ready steering. This preserves
                    // the causal order observed at the backend boundary.
                    event = stream.next(), if !steer_reserved => BackendLoopInput::Event(event),
                    _ = tokio::time::sleep_until(flush_at.into()), if persist_deadline.is_some() => {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                        persist_deadline = None;
                        continue;
                    }
                    approval = pending_backend_approvals.next(), if !pending_backend_approvals.is_empty() => {
                        BackendLoopInput::Approval(
                            approval.expect("non-empty approval queue must yield an outcome")
                        )
                    }
                steer = receive_steer_command(
                    &mut steer_rx,
                    &mut pending_steer,
                    active_vendor_session.is_some(),
                ), if !cancel.is_cancelled() => {
                    let Some(command) = steer else {
                        steer_rx = None;
                        continue;
                    };
                    let materialization_permit = if command.attachment_rows.is_empty() {
                        None
                    } else if let Some(permit) = pending_steer_permit.take() {
                        Some(permit)
                    } else {
                        let lane = self.tool_execution_lock(&session.id);
                        match lane.try_write_owned() {
                            Ok(permit) => Some(permit),
                            Err(_) => {
                                // Wait for actual lane availability as another
                                // select branch. Backend events and approval
                                // outcomes continue to flow while this future
                                // is pending, including the completion that
                                // releases an in-flight vendor mutation.
                                pending_steer = Some(command);
                                steer_mutation_lane_state
                                    .send_replace(SteerMutationLaneState::Waiting);
                                let lane = self.tool_execution_lock(&session.id);
                                pending_steer_lane = Some(
                                    async move { lane.write_owned().await }.boxed(),
                                );
                                consecutive_backend_events = 0;
                                continue;
                            }
                        }
                    };
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    persist_deadline = None;
                    let SteerTurnCommand {
                        content,
                        attachments,
                        attachment_rows,
                        mut attachment_cleanup,
                        response,
                    } = command;
                    // Cancellation can arrive while the selected steer command
                    // flushes pending backend events. Reject it before either
                    // persisting the user message or calling the backend.
                    if cancel.is_cancelled() {
                        let _ = response.send(Err("turn cancelled".into()));
                        continue;
                    }
                    let staged = attachment_rows
                        .iter()
                        .map(|(attachment, path)| AttachmentMaterializationFile {
                            attachment: attachment.clone(),
                            source: PathBuf::from(path),
                        })
                        .collect::<Vec<_>>();
                    // Text-only steering does not touch the worktree and must
                    // reach the active vendor turn even while one of its tools
                    // owns the session mutation lane.
                    let materialized = if staged.is_empty() {
                        Vec::new()
                    } else {
                        let _materialization_permit = materialization_permit
                            .expect("attachment steering must reserve the mutation lane");
                        match self.executor.materialize_attachments(
                            &AttachmentMaterialization {
                                source_root: self.data_dir.join("attachments"),
                                managed_worktree_root: self.data_dir.join("worktrees"),
                                worktree: PathBuf::from(&session.worktree_path),
                                files: staged,
                                cancel: cancel.clone(),
                            },
                        ).await {
                            Ok(materialized) => materialized,
                            Err(error) => {
                                let _ = response.send(Err(error.clone()));
                                bail!("steering attachment materialization failed: {error}");
                            }
                        }
                    };
                    let (images, files): (Vec<_>, Vec<_>) = materialized
                        .into_iter()
                        .partition(|file| file.attachment.mime.starts_with("image/"));
                    let prompt_files = files
                        .iter()
                        .map(|file| (file.attachment.clone(), file.relative_path.clone()))
                        .collect::<Vec<_>>();
                    let backend_prompt = annotate_attachments(content.clone(), &prompt_files);
                    let backend_attachments = images
                        .into_iter()
                        .map(|file| trouve_agents::TurnAttachment {
                            name: file.attachment.name,
                            mime: file.attachment.mime,
                            bytes: file.bytes,
                            local_path: Some(file.absolute_path),
                        })
                        .collect();
                    let payload = match serde_json::to_value(Message::User(backend_prompt.clone())) {
                        Ok(payload) => payload,
                        Err(error) => {
                            let error = anyhow::Error::from(error);
                            let _ = response.send(Err(error.to_string()));
                            return Err(error);
                        }
                    };
                    if let Err(error) = self.store.append_event_with_message(
                        scope.clone(),
                        Event::TurnSteered {
                            turn,
                            content,
                            attachments,
                        },
                        &thread.id,
                        &payload,
                        attachment_rows,
                        attachment_cleanup.claim(),
                    ) {
                        let message = error.to_string();
                        let _ = response.send(Err(message));
                        return Err(error);
                    }
                    attachment_cleanup.disarm();
                    // Durable transcript order is established before the
                    // vendor sees the guidance. A rejection therefore fails
                    // the owning turn instead of erasing accepted input.
                    let backend_result = backend
                        .steer_turn(BackendSteer {
                            cancel: cancel.clone(),
                            session: active_vendor_session
                                .clone()
                                .expect("steering branch requires a backend session"),
                            prompt: backend_prompt.clone(),
                            attachments: backend_attachments,
                        })
                        .await;
                    if let Err(error) = backend_result {
                        let message = error.to_string();
                        let _ = response.send(Err(message.clone()));
                        bail!("backend rejected durable steering input: {message}");
                    }
                    let _ = response.send(Ok(()));
                    consecutive_backend_events = 0;
                    continue;
                }
                }
            };
            let event = match input {
                BackendLoopInput::Event(None) => break,
                BackendLoopInput::Approval(outcome) => {
                    consecutive_backend_events = 0;
                    let BackendApprovalOutcome {
                        owner_thread_id,
                        call_id,
                        responder,
                        approved,
                        mutation_permit,
                    } = outcome;
                    if let Some(owner_thread_id) = owner_thread_id {
                        let Some(collaborator) = collaborators
                            .values_mut()
                            .find(|collaborator| collaborator.thread.id == owner_thread_id)
                        else {
                            let _ = responder.send(false);
                            continue;
                        };
                        collaborator.approval_cancels.remove(&call_id);
                        if collaborator.terminal {
                            let _ = responder.send(false);
                            continue;
                        }
                        let approved = match approved {
                            Ok(approved) => approved,
                            Err(error) => {
                                let _ = responder.send(false);
                                return Err(error);
                            }
                        };
                        if approved {
                            if let Some(permit) = mutation_permit {
                                collaborator
                                    .mutation_permits
                                    .insert(call_id.clone(), permit);
                            }
                            if responder.send(true).is_err() {
                                collaborator.mutation_permits.remove(&call_id);
                            }
                        } else {
                            let _ = responder.send(false);
                        }
                        continue;
                    }
                    backend_approval_cancels.remove(&call_id);
                    let approved = match approved {
                        Ok(approved) => approved,
                        Err(error) => {
                            let _ = responder.send(false);
                            return Err(error);
                        }
                    };
                    if approved {
                        if let Some(permit) = mutation_permit {
                            backend_mutation_permits.insert(call_id.clone(), permit);
                        }
                        if responder.send(true).is_err() {
                            backend_mutation_permits.remove(&call_id);
                        }
                    } else {
                        backend_mutation_permits.remove(&call_id);
                        let _ = responder.send(false);
                    }
                    continue;
                }
                BackendLoopInput::Event(Some(ev)) => match ev {
                    Ok(event) => {
                        consecutive_backend_events = consecutive_backend_events.saturating_add(1);
                        event
                    }
                    Err(BackendError::Cancelled) if cancel.is_cancelled() => break,
                    Err(error) => {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                        for collaborator in collaborators.values_mut() {
                            if !collaborator.terminal {
                                self.finish_backend_collaborator(
                                    session,
                                    backend_id,
                                    collaborator,
                                    Err(format!("parent backend stream failed: {error}")),
                                )
                                .await?;
                            }
                            collaborator_claims.release(&collaborator.thread.id);
                        }
                        deny_pending_backend_approvals(
                            &mut pending_backend_approvals,
                            &mut backend_approval_cancels,
                            &mut collaborators,
                        )
                        .await;
                        reject_pending_steer(
                            &mut pending_steer,
                            "turn ended before steering could be applied",
                        );
                        return Err(anyhow!("backend stream error: {error}"));
                    }
                },
            };
            if first_substantive_event && !matches!(&event, BackendEvent::SessionStarted { .. }) {
                first_substantive_event = false;
                tracing::info!(
                    thread_id = %thread.id,
                    turn,
                    backend = %backend_id,
                    event = backend_event_name(&event),
                    since_turn_started_ms = startup_started.elapsed().as_millis(),
                    "agent startup timing: first vendor event"
                );
            }
            match event {
                BackendEvent::SessionStarted { session_id } => {
                    active_vendor_session = Some(session_id.clone());
                    vendor_threads.insert(session_id.clone(), thread.id.clone());
                    self.bridged_tool_owners
                        .bind_vendor_thread(&thread.id, &session_id, &thread.id)
                        .map_err(anyhow::Error::msg)?;
                    if tools_enabled {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        self.store
                            .set_backend_session(&thread.id, backend_id, &session_id)?;
                    }
                }
                BackendEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    segment.push_str(&delta);
                    persisted.push(Event::AssistantDelta { turn, text: delta });
                }
                BackendEvent::ProgressDelta(delta) => {
                    // Progress is a block boundary like reasoning and tools:
                    // keep answer text on either side in separate bubbles.
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::AssistantProgress { turn, text: delta });
                }
                BackendEvent::ProgressCompleted => {
                    persisted.push(Event::AssistantProgressCompleted { turn });
                }
                BackendEvent::ThinkingDelta(delta) => {
                    // Thinking is a block boundary like a tool call:
                    // finalize the streamed text so far so post-thinking
                    // text starts a new bubble in the right order.
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::AssistantThinking { turn, text: delta });
                }
                BackendEvent::ThinkingCompleted => {
                    persisted.push(Event::AssistantThinkingCompleted { turn });
                }
                BackendEvent::ToolStarted {
                    call_id,
                    tool,
                    mut args,
                } => {
                    if trouve_bridge_wrapper_call(&tool, &args).is_some() {
                        if suppressed_bridge_calls.insert(call_id.clone()) {
                            if let Some(vendor_thread_id) = active_vendor_session.as_deref() {
                                self.announce_trouve_bridge_wrapper(
                                    &thread.id,
                                    vendor_thread_id,
                                    &thread.id,
                                    &call_id,
                                    &tool,
                                    &args,
                                );
                            } else {
                                tracing::warn!(
                                    root_thread_id = %thread.id,
                                    call_id,
                                    "MCP wrapper arrived before its root vendor thread identity"
                                );
                            }
                        }
                        continue;
                    }
                    if strict_tool_free {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        bail!("backend requested tool {tool} during a tool-free turn");
                    }
                    let first_start = seen_tool_cards.insert(call_id.clone());
                    // First-party MCP calls reserve inside handle_tool_call;
                    // Claude mirrors them here under mcp__trouve__*. Native
                    // reads on a backend without a true tool-free mode are
                    // confined but intentionally outside the zero-call cap.
                    if vendor_tool_uses_automated_review_budget(tools_enabled, &tool, first_start) {
                        self.automated_review_tool_budgets.reserve(&thread.id)?;
                    }
                    tool_started_at.insert(call_id.clone(), Instant::now());
                    let could_create = could_request_pull_request_creation(&tool, &args);
                    let mut creation_request = PullRequestCreationRequest::Rejected;
                    if could_create {
                        if let Some((_, owner, repo)) = &github_repository {
                            creation_request =
                                classify_pull_request_creation(&tool, &args, owner, repo);
                        } else {
                            let repository = self
                                .github_repository_for_session(session)
                                .context("discovering repository for pull request creator")?;
                            let (_, owner, repo) = &repository;
                            creation_request =
                                classify_pull_request_creation(&tool, &args, owner, repo);
                            if !matches!(creation_request, PullRequestCreationRequest::Rejected) {
                                github_repository = Some(repository);
                            }
                        }
                    }
                    tool_calls.insert(
                        call_id.clone(),
                        (tool.clone(), args.clone(), creation_request),
                    );
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    // Snippet edits carry no position; the worktree file is
                    // still un-edited at announcement time, so resolve line
                    // hints now for the UI's diff gutter.
                    annotate_edit_lines(Path::new(&session.worktree_path), &mut args);
                    if first_start && !self.tool_card_exists(&thread.id, turn, &call_id) {
                        persisted.push(Event::ToolRequested {
                            turn,
                            call_id: call_id.clone(),
                            tool,
                            args,
                            requires_approval: false,
                        });
                    }
                    persisted.push(Event::ToolStarted { call_id });
                }
                BackendEvent::ToolOutput { call_id, chunk } => {
                    if suppressed_bridge_calls.contains(&call_id) {
                        continue;
                    }
                    if github_repository.is_some()
                        && let Some((_, _, request)) = tool_calls.get(&call_id)
                        && !matches!(request, PullRequestCreationRequest::Rejected)
                    {
                        github_creation_output
                            .entry(call_id.clone())
                            .or_default()
                            .push_str(&chunk);
                    }
                    persisted.push(Event::ToolOutput { call_id, chunk });
                }
                BackendEvent::CommandsUpdated { commands } => {
                    persisted.push(Event::CommandsUpdated { commands });
                }
                BackendEvent::TodosUpdated { todos } => {
                    // Vendor-native plans are authoritative replacements just
                    // like todo_write results, but they are not transcript
                    // tool calls. Persist the thread snapshot and publish the
                    // existing durable event so every client sees the pane.
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    self.store.update_thread_todos(&thread.id, &todos)?;
                    persisted.push(Event::TodosUpdated { todos });
                }
                BackendEvent::UsageUpdated { usage } => {
                    persisted.push(Event::TurnUsageUpdated { turn, usage });
                }
                BackendEvent::CompactionStarted => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::CompactionStarted { turn });
                }
                BackendEvent::CompactionCompleted => {
                    persisted.push(Event::CompactionCompleted {
                        turn,
                        messages_compacted: 0,
                    });
                }
                BackendEvent::CompactionFailed => {
                    persisted.push(Event::CompactionFailed { turn });
                }
                BackendEvent::CollaboratorStarted {
                    session_id,
                    parent_session_id,
                    name,
                    access,
                    prompt,
                    model,
                    thinking_level,
                } => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    let vendor_session_id = session_id.clone();
                    let prompt_announced =
                        prompt.as_deref().is_some_and(|prompt| !prompt.is_empty());
                    self.start_backend_collaborator_claimed(
                        session,
                        thread,
                        backend_id,
                        session_id,
                        &parent_session_id,
                        name,
                        access,
                        prompt,
                        model,
                        thinking_level,
                        &mut collaborator_claims,
                        &mut vendor_threads,
                        &mut collaborators,
                    )
                    .await?;
                    if let Some(owner_thread_id) = vendor_threads.get(&vendor_session_id) {
                        self.bridged_tool_owners
                            .bind_vendor_thread(&thread.id, &vendor_session_id, owner_thread_id)
                            .map_err(anyhow::Error::msg)?;
                    }
                    self.publish_backend_collaborator_spawn(
                        thread,
                        turn,
                        &vendor_session_id,
                        &mut collaborators,
                    )
                    .await?;
                    // The child route does not replay its initial user item.
                    // When the backend announcement supplies or recovers the
                    // spawn prompt, publish it immediately instead of waiting
                    // for the child's first tool completion or terminal event.
                    if prompt_announced
                        && let Some(collaborator) = collaborators.get_mut(&vendor_session_id)
                    {
                        flush_backend_event_batch(
                            &self.store,
                            &Scope::Thread(collaborator.thread.id.clone()),
                            &mut collaborator.persisted,
                        )
                        .await?;
                    }
                }
                BackendEvent::CollaboratorEvent {
                    session_id,
                    turn_id,
                    event,
                } => {
                    if !collaborators.contains_key(&session_id) {
                        let parent_session_id = active_vendor_session
                            .as_deref()
                            .unwrap_or_default()
                            .to_string();
                        self.start_backend_collaborator_claimed(
                            session,
                            thread,
                            backend_id,
                            session_id.clone(),
                            &parent_session_id,
                            None,
                            BackendCollaboratorAccess::Inherit,
                            None,
                            None,
                            None,
                            &mut collaborator_claims,
                            &mut vendor_threads,
                            &mut collaborators,
                        )
                        .await?;
                    }
                    if let Some(owner_thread_id) = vendor_threads.get(&session_id) {
                        self.bridged_tool_owners
                            .bind_vendor_thread(&thread.id, &session_id, owner_thread_id)
                            .map_err(anyhow::Error::msg)?;
                    }
                    if let Some(collaborator) = collaborators.get(&session_id)
                        && !collaborator_claims.claim(&collaborator.thread.id, &session.id)
                    {
                        bail!(
                            "cannot route provider collaborator {} while another turn owns it",
                            collaborator.thread.id
                        );
                    }
                    let completed_successfully =
                        matches!(&event, BackendCollaboratorEvent::Completed { .. });
                    let terminal_thread =
                        if let Some(collaborator) = collaborators.get_mut(&session_id) {
                            self.prepare_backend_collaborator_turn(
                                session,
                                backend_id,
                                collaborator,
                                turn_id.as_deref(),
                            )
                            .await?;
                            if !self.suppress_collaborator_bridge_wrapper(
                                &thread.id,
                                &session_id,
                                collaborator,
                                &event,
                            ) {
                                self.persist_backend_collaborator_event(
                                    session,
                                    mode,
                                    backend_id,
                                    collaborator,
                                    event,
                                    &cancel,
                                )
                                .await?;
                            }
                            if let Some(approval) = collaborator.pending_approval.take() {
                                let owner_thread_id = approval.thread.id.clone();
                                let approval_call_id = approval.call_id.clone();
                                let approval_cancel = cancel.child_token();
                                collaborator
                                    .approval_cancels
                                    .insert(approval_call_id, approval_cancel.clone());
                                pending_backend_approvals.push(self.pending_backend_approval(
                                    session.clone(),
                                    approval.thread,
                                    approval.turn,
                                    effective_read_only || approval.mode.read_only,
                                    approval.call_id,
                                    approval.tool,
                                    approval.args,
                                    approval.responder,
                                    approval_cancel,
                                    // Full-bridge calls acquire the lane in
                                    // handle_tool_call; vendor-native child
                                    // mutations need it here.
                                    !full_tool_bridge,
                                    Some(owner_thread_id),
                                ));
                            }
                            collaborator
                                .terminal
                                .then(|| collaborator.thread.id.clone())
                        } else {
                            None
                        };
                    self.publish_backend_collaborator_spawn(
                        thread,
                        turn,
                        &session_id,
                        &mut collaborators,
                    )
                    .await?;
                    if let Some(thread_id) = terminal_thread {
                        collaborator_claims.release(&thread_id);
                        if completed_successfully {
                            self.dispatch_queue(&thread_id)
                                .map_err(|error| anyhow!(error.to_string()))?;
                        }
                    }
                }
                BackendEvent::ToolCompleted {
                    call_id,
                    ok,
                    result,
                } => {
                    if suppressed_bridge_calls.remove(&call_id) {
                        // The bridged execution path owns persistence and PR
                        // evidence for this duplicate vendor lifecycle card.
                        backend_mutation_permits.remove(&call_id);
                        continue;
                    }
                    let status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Error
                    };
                    let execution_duration_ms =
                        tool_started_at.remove(&call_id).map(monotonic_elapsed_ms);
                    let todos = match tool_calls.get(&call_id) {
                        Some((tool, args, _)) => self.persist_todos_from_result(
                            &thread.id,
                            tool,
                            status,
                            &result,
                            Some(args),
                        )?,
                        None => None,
                    };
                    let mut verification = None;
                    if ok
                        && let Some(repository @ (host, owner, repo)) = &github_repository
                        && let Some((_, _, request)) = tool_calls.get(&call_id)
                    {
                        if !matches!(request, PullRequestCreationRequest::Rejected) {
                            let result_numbers = pr_numbers_in_value(&result, host, owner, repo);
                            let creation_output =
                                github_creation_output.remove(&call_id).unwrap_or_default();
                            let output_numbers = crate::github::pr_numbers_in_text(
                                &creation_output,
                                host,
                                owner,
                                repo,
                            );
                            if matches!(request, PullRequestCreationRequest::Confirmed)
                                || (matches!(request, PullRequestCreationRequest::Unresolved)
                                    && (!result_numbers.is_empty() || !output_numbers.is_empty()))
                            {
                                verification =
                                    Some((repository.clone(), result_numbers, output_numbers));
                            }
                        }
                        github_creation_output.remove(&call_id);
                    } else {
                        github_creation_output.remove(&call_id);
                    }
                    // Approval-gated vendor creators retain the session write
                    // lane until this post-execution attestation is captured.
                    // A provider-owned call that executed without that gate is
                    // completed normally but cannot authorize a session PR:
                    // acquiring a speculative permit after ToolStarted cannot
                    // serialize execution that has already begun.
                    let evidence = if verification.is_some() {
                        let evidence = if backend_mutation_permits.contains_key(&call_id) {
                            Self::capture_session_pr_head(session).await
                        } else {
                            None
                        };
                        if evidence.is_none() {
                            tracing::warn!(
                                session_id = session.id,
                                %call_id,
                                "cannot capture immutable PR ownership evidence"
                            );
                        }
                        evidence
                    } else {
                        None
                    };
                    backend_mutation_permits.remove(&call_id);

                    let mut completion_events = vec![Event::ToolCompleted {
                        call_id,
                        status,
                        result,
                        execution_duration_ms,
                    }];
                    if let Some(todos) = todos {
                        completion_events.push(Event::TodosUpdated { todos });
                    }
                    if let Some((repository, priority_numbers, fallback_numbers)) = verification {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        let intents = Self::session_pr_verification_intents(
                            session,
                            repository,
                            priority_numbers,
                            fallback_numbers,
                            evidence,
                        );
                        self.store
                            .append_events_with_session_pr_verification_intents(
                                scope.clone(),
                                completion_events,
                                intents.clone(),
                            )
                            .await?;
                        if !intents.is_empty() {
                            self.session_pr_verification_wake.notify_one();
                        }
                    } else {
                        persisted.extend(completion_events);
                    }
                }
                BackendEvent::ApprovalNeeded {
                    call_id,
                    tool,
                    args,
                    responder,
                } => {
                    if strict_tool_free {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        bail!("backend requested approval for {tool} during a tool-free turn");
                    }
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    let approval_cancel = cancel.child_token();
                    backend_approval_cancels.insert(call_id.clone(), approval_cancel.clone());
                    pending_backend_approvals.push(self.pending_backend_approval(
                        session.clone(),
                        thread.clone(),
                        turn,
                        effective_read_only,
                        call_id,
                        tool,
                        args,
                        responder,
                        approval_cancel,
                        // Full-bridge calls acquire the same lane inside
                        // handle_tool_call; pre-acquiring here would make the
                        // vendor wait on its own permit. Native Codex tools
                        // are read-only under the full-bridge sandbox.
                        !full_tool_bridge,
                        None,
                    ));
                    continue;
                }
                BackendEvent::QuestionsNeeded {
                    request_id,
                    title,
                    questions,
                    responder,
                } => {
                    // Vendor question extensions are another engine-served
                    // interaction path. Reserve before publishing or waiting
                    // so they share the same hard review-turn allowance.
                    self.automated_review_tool_budgets.reserve(&thread.id)?;
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    let answers = self
                        .ask_user_questions(
                            &thread.id,
                            turn,
                            &request_id,
                            title,
                            questions,
                            &cancel,
                        )
                        .await?;
                    let _ = responder.send(answers);
                }
                BackendEvent::Completed { usage } => {
                    usage_total.input_tokens += usage.input_tokens;
                    usage_total.output_tokens += usage.output_tokens;
                    usage_total.cached_input_tokens += usage.cached_input_tokens;
                    if let Some(cost) = usage.cost_usd {
                        usage_total.cost_usd = Some(usage_total.cost_usd.unwrap_or(0.0) + cost);
                    }
                    if usage.context_window.is_some() {
                        usage_total.context_window = usage.context_window;
                    }
                    if usage.context_input_tokens.is_some() {
                        usage_total.context_input_tokens = usage.context_input_tokens;
                    }
                }
            }
            let collaborator_pending = collaborators
                .values()
                .any(|collaborator| !collaborator.persisted.is_empty());
            if persisted.is_empty() && !collaborator_pending {
                persist_deadline = None;
            } else if persisted.len()
                + collaborators
                    .values()
                    .map(|collaborator| collaborator.persisted.len())
                    .sum::<usize>()
                >= STREAM_EVENT_BATCH_MAX
            {
                flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                persist_deadline = None;
            } else if persist_deadline.is_none() {
                persist_deadline = Some(Instant::now() + STREAM_EVENT_BATCH_WINDOW);
            }
        }
        // Adapters close only after cancellation cleanup has completed. The
        // stream can now be dropped without racing a replacement vendor turn.
        drop(stream);
        let pending_steer_reason = if cancel.is_cancelled() {
            "turn cancelled"
        } else {
            "turn ended before steering could be applied"
        };
        reject_pending_steer(&mut pending_steer, pending_steer_reason);
        steer_mutation_lane_state.send_replace(SteerMutationLaneState::Ended);
        drop(pending_steer_lane.take());
        drop(pending_steer_permit.take());
        // Vendors may omit ToolCompleted on cancellation or protocol failure.
        // Never carry their mutation lease into flush, checkpoint, or terminal
        // turn bookkeeping.
        backend_mutation_permits.clear();
        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
        flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;

        for collaborator in collaborators.values_mut() {
            if !collaborator.terminal {
                let reason = if cancel.is_cancelled() {
                    "turn cancelled".to_string()
                } else {
                    "parent turn ended before collaborator completion".to_string()
                };
                self.finish_backend_collaborator(session, backend_id, collaborator, Err(reason))
                    .await?;
            }
            collaborator_claims.release(&collaborator.thread.id);
        }
        deny_pending_backend_approvals(
            &mut pending_backend_approvals,
            &mut backend_approval_cancels,
            &mut collaborators,
        )
        .await;

        if cancel.is_cancelled() {
            if !segment.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: segment,
                    },
                )?;
            }
            if !text.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            return Ok(());
        }

        if !segment.is_empty() {
            self.store.append_event(
                scope.clone(),
                Event::AssistantMessage {
                    turn,
                    content: segment,
                },
            )?;
        }
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::Assistant {
                content: text,
                tool_calls: Vec::new(),
                reasoning: Vec::new(),
            })?,
        )?;
        if tools_enabled {
            let seen_after = self.store.messages(&thread.id)?.len() as u64;
            self.store
                .mark_backend_seen(&thread.id, backend_id, seen_after)?;
        }

        // Vendor turns can contain several model calls. Their final usage is
        // aggregate billing data, while context_input_tokens tracks only the
        // newest call's provider-authoritative context measurement.
        let context_input_tokens = usage_total.context_input_tokens.unwrap_or_else(|| {
            usage_total
                .input_tokens
                .saturating_add(usage_total.cached_input_tokens)
        });
        usage_total.context_input_tokens = Some(context_input_tokens);
        self.store.record_usage(
            &session.id,
            &thread.id,
            turn,
            &thread.model,
            &usage_total,
            context_input_tokens,
        )?;
        // Read-only turns never checkpoint shared dirt. Mutation-capable
        // turns serialize the snapshot with every other worktree mutation.
        let checkpoint_id = if mode.read_only {
            None
        } else {
            self.maybe_checkpoint(session, thread, turn, &cancel)
                .await?
        };
        if cancel.is_cancelled() {
            return Ok(());
        }
        self.store.append_event(
            scope,
            Event::TurnCompleted {
                turn,
                usage: usage_total,
                checkpoint_id,
            },
        )?;
        Ok(())
    }

    fn backend_tool_mutates(&self, tool: &str) -> bool {
        // Bridged trouve tools have authoritative metadata. Vendor-native
        // permission requests are conservatively mutations: vendors request
        // approval specifically for actions with side effects.
        crate::mcp::split_tool_name(tool)
            .filter(|(server, _)| *server == "trouve")
            .and_then(|(_, name)| self.executor.tool_mutates(name))
            .unwrap_or(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn pending_backend_approval(
        self: &Arc<Self>,
        session: Session,
        thread: Thread,
        turn: u64,
        effective_read_only: bool,
        call_id: String,
        tool: String,
        args: serde_json::Value,
        responder: tokio::sync::oneshot::Sender<bool>,
        cancel: tokio_util::sync::CancellationToken,
        acquire_mutation_permit: bool,
        owner_thread_id: Option<String>,
    ) -> futures::future::BoxFuture<'static, BackendApprovalOutcome> {
        let engine = self.clone();
        async move {
            let mut approved = engine
                .gate_backend_approval(
                    &session,
                    &thread,
                    turn,
                    effective_read_only,
                    &call_id,
                    &tool,
                    &args,
                    &cancel,
                )
                .await;
            let mut mutation_permit = None;
            if !effective_read_only
                && acquire_mutation_permit
                && approved.as_ref().is_ok_and(|approved| *approved)
                && engine.backend_tool_mutates(&tool)
            {
                let lock = engine.tool_execution_lock(&session.id);
                match tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    permit = lock.write_owned() => Some(permit),
                } {
                    Some(permit) => mutation_permit = Some(permit),
                    None => approved = Ok(false),
                }
            }
            BackendApprovalOutcome {
                owner_thread_id,
                call_id,
                responder,
                approved,
                mutation_permit,
            }
        }
        .boxed()
    }

    /// Gate one backend approval request through trouve's permission layer:
    /// allow-list hits auto-approve, read-only personas deny, otherwise ask the
    /// user through the ApprovalHub (same endpoints as native tool calls).
    #[allow(clippy::too_many_arguments)]
    async fn gate_backend_approval(
        &self,
        session: &Session,
        thread: &Thread,
        turn: u64,
        effective_read_only: bool,
        call_id: &str,
        tool: &str,
        args: &serde_json::Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<bool> {
        // A vendor write aimed outside the session worktree is denied
        // without asking: the vendor executes the tool itself, so this is
        // the only place trouve can stop an edit from escaping into some
        // other checkout, and the approval card may render the path
        // worktree-relative — the user could approve without noticing.
        if let Some(path) =
            crate::permissions::escaping_write_path(tool, args, Path::new(&session.worktree_path))
        {
            tracing::warn!(
                "denied vendor tool {tool}: {path} is outside worktree {}",
                session.worktree_path
            );
            return Ok(false);
        }
        let scope = Scope::Thread(thread.id.clone());
        let key = allow_key(tool, args);
        // Bridged trouve tools are our own: trust the executor's mutability
        // flag so read-only tools (code search) pass even in read-only
        // modes. Anything else the vendor asks about is treated as mutating
        // (it only asks for things it considers mutating).
        let mutates = self.backend_tool_mutates(tool);
        let decision = gate(
            thread.permission_mode,
            effective_read_only,
            mutates,
            &self.approvals.allow_list(&session.id),
            &key,
        );
        match decision {
            Gate::Allow => Ok(true),
            Gate::Deny => Ok(false),
            Gate::NeedsApproval => {
                // Cursor (and occasionally Codex) can ask for permission
                // before the tool_call announcement that normally creates the
                // card. Without a synthetic card the Approve/Deny UI has
                // nowhere to attach and the turn hangs forever.
                let mut approval_events = Vec::with_capacity(2);
                if !self.tool_card_exists(&thread.id, turn, call_id) {
                    let mut display_args = args.clone();
                    annotate_edit_lines(Path::new(&session.worktree_path), &mut display_args);
                    approval_events.push(Event::ToolRequested {
                        turn,
                        call_id: call_id.to_string(),
                        tool: tool.to_string(),
                        args: display_args,
                        requires_approval: true,
                    });
                }
                let rx = self
                    .approvals
                    .request(&thread.id, call_id)
                    .with_context(|| {
                        format!(
                            "duplicate pending approval {call_id} in thread {}",
                            thread.id
                        )
                    })?;
                // Arm cleanup immediately after registration. There is no
                // await before this guard exists, so dropping the future can
                // never orphan a pending sender.
                let mut cleanup = PendingApprovalCleanup {
                    approvals: self.approvals.clone(),
                    store: self.store.clone(),
                    scope: scope.clone(),
                    thread_id: thread.id.clone(),
                    call_id: call_id.to_string(),
                    armed: true,
                    requested_persisted: false,
                };
                approval_events.push(Event::ApprovalRequested {
                    turn,
                    call_id: call_id.to_string(),
                });
                self.store.append_events(scope.clone(), approval_events)?;
                cleanup.requested_persisted = true;
                // A cancelled turn must not hang on an unanswered approval.
                let decision = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        // Remove the pending sender so a late HTTP approval
                        // cannot target a turn that has entered cleanup.
                        let _ = self.approvals.resolve(
                            &thread.id,
                            call_id,
                            ApprovalDecision::Deny,
                        );
                        ApprovalDecision::Deny
                    },
                    d = rx => d.unwrap_or(ApprovalDecision::Deny),
                };
                // Cancellation owns the terminal outcome. A user response
                // racing cleanup must not approve work or broaden policy.
                let decision = if cancel.is_cancelled() {
                    ApprovalDecision::Deny
                } else {
                    decision
                };
                self.store.append_event(
                    scope,
                    Event::ApprovalResolved {
                        call_id: call_id.to_string(),
                        decision,
                    },
                )?;
                cleanup.armed = false;
                let unlocks_mcp_server =
                    decision == ApprovalDecision::Approve && key.starts_with("mcp:");
                if !cancel.is_cancelled()
                    && (decision == ApprovalDecision::AlwaysApprove || unlocks_mcp_server)
                {
                    // MCP approval is first-use per server and session: a
                    // plain approval unlocks this server, matching native MCP
                    // calls without broadening approval to other servers.
                    self.approvals.extend_allow_list(&session.id, key);
                }
                Ok(decision != ApprovalDecision::Deny && !cancel.is_cancelled())
            }
        }
    }

    /// Summarize the transcript into a single message when its estimated
    /// size crosses `COMPACTION_THRESHOLD` of the model's context window.
    async fn maybe_compact(
        &self,
        thread: &Thread,
        turn: u64,
        provider: &Arc<dyn Provider>,
        model_name: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        // The live listing knows gateway models (kilocode, openrouter, ...)
        // the static catalog doesn't; it is cached, so this is cheap. Never
        // compact against a guessed window: an early lossy summary is worse
        // than surfacing that the provider omitted the required metadata.
        let live = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            models = provider.list_models() => models,
        };
        let known = provider.models();
        let Some(context_window) = live
            .iter()
            .chain(known.iter())
            .find(|m| m.id == thread.model)
            .map(|m| m.context_window)
            .filter(|w| *w > 0)
        else {
            if self
                .compaction_warnings
                .lock()
                .unwrap()
                .insert(thread.model.clone())
            {
                tracing::warn!(
                    model = %thread.model,
                    "automatic compaction disabled for this model: provider did not report a context window"
                );
            }
            return Ok(());
        };
        let payloads = self.store.messages(&thread.id)?;
        if payloads.len() < 2 {
            return Ok(());
        }
        // Prefer the provider-reported size of the last request; fall back
        // to the standard ~4 chars/token estimate over the raw transcript.
        let estimated_tokens = self.store.last_input_tokens(&thread.id)?.unwrap_or(0).max(
            payloads
                .iter()
                .map(|p| p.to_string().len() as u64)
                .sum::<u64>()
                / 4,
        );
        if (estimated_tokens as f64) < COMPACTION_THRESHOLD * context_window as f64 {
            return Ok(());
        }

        let scope = Scope::Thread(thread.id.clone());
        self.store
            .append_event(scope.clone(), Event::CompactionStarted { turn })?;

        let mut messages: Vec<Message> = vec![Message::System(
            "You are compacting an AI coding session transcript. Produce a dense summary \
             that preserves: the user's goals and constraints, decisions made, files \
             created/modified and how, commands run and their outcomes, current state, \
             unresolved problems, and what should happen next. Write it so the assistant \
             can seamlessly continue the session from the summary alone."
                .into(),
        )];
        for payload in &payloads {
            messages.push(serde_json::from_value(payload.clone())?);
        }
        messages = sanitize_transcript(messages);
        messages.push(Message::User(
            "Summarize the conversation so far per your instructions.".into(),
        ));

        let empty_options = serde_json::Map::new();
        let stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                self.store.append_event(scope, Event::CompactionFailed { turn })?;
                return Ok(());
            }
            stream = provider.stream_chat(model_name, &messages, &[], &empty_options) => {
                stream.map_err(|e| anyhow!("compaction provider error: {e}"))?
            }
        };
        let mut stream = stream;
        stream = trouve_providers::coalesce_event_stream(stream);
        let mut summary = String::new();
        loop {
            let ev = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    self.store.append_event(scope, Event::CompactionFailed { turn })?;
                    return Ok(());
                }
                ev = stream.next() => match ev {
                    Some(ev) => ev,
                    None => break,
                }
            };
            if let ProviderEvent::TextDelta(delta) =
                ev.map_err(|e| anyhow!("compaction stream error: {e}"))?
            {
                summary.push_str(&delta);
            }
        }
        if summary.trim().is_empty() {
            anyhow::bail!("compaction produced an empty summary");
        }

        let replacement = serde_json::to_value(Message::User(format!(
            "[Context was compacted. Older turns were summarized below; exact details \
             (error text, file paths, command output) are recoverable with the \
             search_transcript tool.]\n\n{summary}"
        )))?;
        self.store.replace_messages(&thread.id, &[replacement])?;
        self.store.append_event(
            scope,
            Event::CompactionCompleted {
                turn,
                messages_compacted: payloads.len() as u64,
            },
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_tool_calls_parallel(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
        ctx: &ToolCtx,
        calls: Vec<trouve_providers::ToolCallRequest>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Vec<(String, NativeToolCallResult)> {
        let mut results = Vec::with_capacity(calls.len());
        let mut read_batch = Vec::new();
        for (index, call) in calls.into_iter().enumerate() {
            let definitely_read_only = call.name != "ask_question"
                && self.executor.tool_mutates(&call.name) == Some(false);
            if definitely_read_only {
                read_batch.push((index, call));
                continue;
            }
            if !read_batch.is_empty() {
                results.extend(
                    self.handle_native_tool_batch(
                        session,
                        thread,
                        turn,
                        mode,
                        ctx,
                        std::mem::take(&mut read_batch),
                        cancel,
                    )
                    .await,
                );
            }
            // Mutating, interactive, and unknown calls are FIFO barriers.
            // Awaiting each one prevents a later mutation from acquiring the
            // session lane first and prevents reads from crossing it.
            let call_id = call.id.clone();
            let result = self
                .handle_tool_call(session, thread, turn, mode, ctx, &call, cancel)
                .await;
            results.push((index, call_id, result));
        }
        if !read_batch.is_empty() {
            results.extend(
                self.handle_native_tool_batch(session, thread, turn, mode, ctx, read_batch, cancel)
                    .await,
            );
        }
        results.sort_unstable_by_key(|(index, _, _)| *index);
        results
            .into_iter()
            .map(|(_, call_id, result)| (call_id, result))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_native_tool_batch(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
        ctx: &ToolCtx,
        calls: Vec<(usize, trouve_providers::ToolCallRequest)>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Vec<(usize, String, NativeToolCallResult)> {
        futures::stream::iter(calls.into_iter().map(|(index, call)| {
            let engine = self.clone();
            let session = session.clone();
            let thread = thread.clone();
            let mode = mode.clone();
            let ctx = ctx.clone();
            let cancel = cancel.clone();
            async move {
                let call_id = call.id.clone();
                let result = engine
                    .handle_tool_call(&session, &thread, turn, &mode, &ctx, &call, &cancel)
                    .await;
                (index, call_id, result)
            }
        }))
        .buffer_unordered(MAX_PARALLEL_TOOL_CALLS)
        .collect::<Vec<_>>()
        .await
    }

    /// Gate, (maybe) get approval for, and execute one tool call. Returns the
    /// content fed back to the model.
    #[allow(clippy::too_many_arguments)]
    async fn handle_tool_call(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
        ctx: &ToolCtx,
        call: &trouve_providers::ToolCallRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(String, Vec<trouve_providers::ToolImage>)> {
        // The disposable review task arms this authority before dispatching
        // its prompt. Reserve synchronously before any engine-served wait,
        // durable tool event, approval, or ToolExecutor future can begin.
        self.automated_review_tool_budgets.reserve(&thread.id)?;
        let scope = Scope::Thread(thread.id.clone());
        let call_id = if call.id.is_empty() {
            new_id("call")
        } else {
            call.id.clone()
        };

        let engine_served = matches!(
            call.name.as_str(),
            "ask_question"
                | "spawn_thread"
                | "spawn_session"
                | "spawn_output"
                | "search_transcript"
        );
        if engine_served && !personas::tool_allowed(mode, &call.name) {
            return Ok((
                "Tool call denied: not permitted in this mode.".into(),
                Vec::new(),
            ));
        }

        // ask_question is engine-served (it blocks on the QuestionHub, which
        // tools can't reach). No tool card is emitted: the question wizard is
        // its representation in the UI.
        if call.name == "ask_question" {
            let result = match parse_question_args(&call.arguments) {
                Ok((title, questions)) => {
                    let answers = self
                        .ask_user_questions(
                            &thread.id,
                            turn,
                            &call_id,
                            title,
                            questions.clone(),
                            cancel,
                        )
                        .await?;
                    question_result_json(&questions, answers)
                }
                Err(e) => serde_json::json!({ "error": e }),
            };
            return Ok((result.to_string(), Vec::new()));
        }

        // The spawn family and transcript search are engine-served too
        // (child agents and cross-thread history need the store and turn
        // dispatch, which tools can't reach). Unlike ask_question they do
        // get tool cards — these are real, visible actions. Errors become
        // tool results, never turn failures.
        if matches!(
            call.name.as_str(),
            "spawn_thread" | "spawn_session" | "spawn_output" | "search_transcript"
        ) {
            self.store
                .append_events_async(
                    scope.clone(),
                    vec![
                        Event::ToolRequested {
                            turn,
                            call_id: call_id.clone(),
                            tool: call.name.clone(),
                            args: call.arguments.clone(),
                            requires_approval: false,
                        },
                        Event::ToolStarted {
                            call_id: call_id.clone(),
                        },
                    ],
                )
                .await?;
            let execution_started = std::time::Instant::now();
            let outcome = if call.name == "search_transcript" {
                self.handle_search_transcript(session, thread, &call.arguments)
            } else {
                self.handle_spawn_tool(session, thread, mode, &call.name, &call.arguments, cancel)
                    .await
            };
            let execution_duration_ms = monotonic_elapsed_ms(execution_started);
            let result = match outcome {
                Ok(v) => v,
                Err(e) => serde_json::json!({ "error": e.to_string() }),
            };
            let status = if result.get("error").is_some() {
                ToolStatus::Error
            } else {
                ToolStatus::Ok
            };
            let model_result = result.to_string();
            let spawned = if status == ToolStatus::Ok
                && matches!(call.name.as_str(), "spawn_thread" | "spawn_session")
            {
                match (
                    result.get("thread_id").and_then(serde_json::Value::as_str),
                    result.get("session_id").and_then(serde_json::Value::as_str),
                    result.get("prompt").and_then(serde_json::Value::as_str),
                    result.get("model").and_then(serde_json::Value::as_str),
                ) {
                    (Some(thread_id), Some(session_id), Some(prompt), Some(model)) => Some((
                        thread_id.to_string(),
                        session_id.to_string(),
                        prompt.to_string(),
                        model.to_string(),
                    )),
                    _ => None,
                }
            } else {
                None
            };
            let mut completed = vec![Event::ToolCompleted {
                call_id: call_id.clone(),
                status,
                result,
                execution_duration_ms: Some(execution_duration_ms),
            }];
            if let Some((thread_id, session_id, prompt, model)) = spawned {
                completed.push(Event::SubagentSpawned {
                    turn,
                    thread_id,
                    session_id,
                    prompt,
                    model,
                    call_id: Some(call_id),
                });
            }
            self.store.append_events_async(scope, completed).await?;
            return Ok((model_result, Vec::new()));
        }

        let known = self.executor.tool_mutates(&call.name);
        let allowed_by_mode =
            mode.allowed_tools.is_empty() || mode.allowed_tools.contains(&call.name);
        let mutates = known.unwrap_or(true);
        let key = allow_key(&call.name, &call.arguments);
        let decision = if known.is_none() || !allowed_by_mode {
            Gate::Deny
        } else {
            gate(
                thread.permission_mode,
                mode.read_only,
                mutates,
                &self.approvals.allow_list(&session.id),
                &key,
            )
        };

        // Display copy of the args: snippet edits (edit_file) pick up a
        // "_line" hint locating the old text in the pre-edit file, so the
        // UI diff can number its gutter. Stored/executed args stay pristine.
        let mut display_args = call.arguments.clone();
        annotate_edit_lines(Path::new(&session.worktree_path), &mut display_args);
        let requested_event = Event::ToolRequested {
            turn,
            call_id: call_id.clone(),
            tool: call.name.clone(),
            args: display_args,
            requires_approval: decision == Gate::NeedsApproval,
        };
        if decision != Gate::NeedsApproval {
            self.store
                .append_event(scope.clone(), requested_event.clone())?;
        }

        let decision = match decision {
            Gate::Deny => {
                self.store
                    .append_event_async(
                        scope.clone(),
                        Event::ToolCompleted {
                            call_id: call_id.clone(),
                            status: ToolStatus::Denied,
                            result: serde_json::json!({
                                "error": "tool not permitted in this mode"
                            }),
                            execution_duration_ms: None,
                        },
                    )
                    .await?;
                return Ok((
                    "Tool call denied: not permitted in this mode.".into(),
                    Vec::new(),
                ));
            }
            Gate::NeedsApproval => {
                let rx = self
                    .approvals
                    .request(&thread.id, &call_id)
                    .with_context(|| {
                        format!(
                            "duplicate pending approval {call_id} in thread {}",
                            thread.id
                        )
                    })?;
                let mut cleanup = PendingApprovalCleanup {
                    approvals: self.approvals.clone(),
                    store: self.store.clone(),
                    scope: scope.clone(),
                    thread_id: thread.id.clone(),
                    call_id: call_id.clone(),
                    armed: true,
                    requested_persisted: false,
                };
                self.store.append_events(
                    scope.clone(),
                    vec![
                        requested_event,
                        Event::ApprovalRequested {
                            turn,
                            call_id: call_id.clone(),
                        },
                    ],
                )?;
                cleanup.requested_persisted = true;
                // A cancelled turn must not hang on an unanswered approval:
                // treat cancellation as a denial so the wait unblocks.
                let decision = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        // Remove the pending sender so a late HTTP approval
                        // cannot target a turn that has entered cleanup.
                        let _ = self.approvals.resolve(
                            &thread.id,
                            &call_id,
                            ApprovalDecision::Deny,
                        );
                        ApprovalDecision::Deny
                    },
                    d = rx => d.unwrap_or(ApprovalDecision::Deny),
                };
                let decision = if cancel.is_cancelled() {
                    ApprovalDecision::Deny
                } else {
                    decision
                };
                self.store.append_event(
                    scope.clone(),
                    Event::ApprovalResolved {
                        call_id: call_id.clone(),
                        decision,
                    },
                )?;
                cleanup.armed = false;
                let unlocks_mcp_server =
                    decision == ApprovalDecision::Approve && key.starts_with("mcp:");
                if !cancel.is_cancelled()
                    && (decision == ApprovalDecision::AlwaysApprove || unlocks_mcp_server)
                {
                    // MCP approval is per server per session (first use).
                    self.approvals.extend_allow_list(&session.id, key);
                }
                decision
            }
            Gate::Allow => ApprovalDecision::Approve,
        };

        if decision == ApprovalDecision::Deny || cancel.is_cancelled() {
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::ToolCompleted {
                        call_id: call_id.clone(),
                        status: ToolStatus::Denied,
                        result: serde_json::json!({"error": "denied by user"}),
                        execution_duration_ms: None,
                    },
                )
                .await?;
            return Ok(("Tool call denied by the user.".into(), Vec::new()));
        }

        // Repository discovery and classification do not depend on the tool
        // result. Resolve them before taking the mutation lane so completion
        // only needs to extract returned PR numbers and snapshot HEAD.
        let pr_creation = if could_request_pull_request_creation(&call.name, &call.arguments) {
            let repository = self
                .github_repository_for_session(session)
                .context("discovering repository for pull request creator")?;
            let (_, owner, repo) = &repository;
            let request = classify_pull_request_creation(&call.name, &call.arguments, owner, repo);
            (!matches!(request, PullRequestCreationRequest::Rejected))
                .then_some((repository, request))
        } else {
            None
        };

        enum ExecutionPermit {
            Read {
                _guard: tokio::sync::OwnedRwLockReadGuard<()>,
            },
            Write {
                guard: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
            },
            /// Background-job control must be able to poll or terminate a
            /// process while that process retains the write lane.
            BackgroundControl,
        }
        let execution_lock = self.tool_execution_lock(&session.id);
        let permit = if matches!(call.name.as_str(), "shell_output" | "shell_kill") {
            Some(ExecutionPermit::BackgroundControl)
        } else if mutates || pr_creation.is_some() {
            // A confirmed or unresolved creator must hold the write lane even
            // if its tool metadata incorrectly labels it read-only: the exact
            // branch/HEAD snapshot is durable authorization evidence.
            tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                permit = execution_lock.clone().write_owned() => Some(ExecutionPermit::Write { guard: Some(permit) }),
            }
        } else {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                permit = execution_lock.clone().read_owned() => Some(ExecutionPermit::Read { _guard: permit }),
            }
        };
        let (mut outcome, execution_duration_ms, verification) = if let Some(permit) = permit {
            // Retain the lane until the executor has acknowledged cancellation
            // and cleaned up its process/protocol resources.
            let mut permit = Some(permit);
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::ToolStarted {
                        call_id: call_id.clone(),
                    },
                )
                .await?;
            let executor = self.executor.clone();
            let mut tool_ctx = ctx.clone();
            if call.name == "shell"
                && call
                    .arguments
                    .get("run_in_background")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                && let Some(ExecutionPermit::Write { guard }) = permit.as_mut()
                && let Some(guard) = guard.take()
            {
                tool_ctx.background_mutation_lease =
                    Some(Arc::new(BackgroundMutationLease::new(guard)));
            }
            let tool_name = call.name.clone();
            let tool_arguments = call.arguments.clone();
            let execution_started = std::time::Instant::now();
            let mut execute = Box::pin(async move {
                executor
                    .execute(&tool_ctx, &tool_name, &tool_arguments)
                    .await
            });
            let outcome = tokio::select! {
                biased;
                result = &mut execute => result,
                _ = cancel.cancelled() => {
                    match tokio::time::timeout(TOOL_CANCEL_CLEANUP_TIMEOUT, &mut execute).await {
                        Ok(result) => result,
                        Err(_) => {
                            tracing::warn!(
                                tool = %call.name,
                                call_id,
                                "tool executor did not acknowledge cancellation within {}s",
                                TOOL_CANCEL_CLEANUP_TIMEOUT.as_secs(),
                            );
                            // A custom executor may violate ToolCtx's cleanup
                            // contract. Do not let it hold the turn terminal
                            // state forever, but quarantine its execution lane
                            // until it eventually returns so replacement work
                            // cannot race a still-live mutation.
                            let quarantine = permit
                                .take()
                                .expect("an executing tool owns its session lane");
                            let tool = call.name.clone();
                            let call_id = call_id.clone();
                            tokio::spawn(async move {
                                let _quarantine = quarantine;
                                let _ = execute.await;
                                tracing::warn!(
                                    tool,
                                    call_id,
                                    "late tool cancellation cleanup completed; session lane released",
                                );
                            });
                            ToolResult::error("tool cancellation cleanup timed out")
                        }
                    }
                }
            };
            let retains_creation_lane = matches!(
                permit.as_ref(),
                Some(ExecutionPermit::Write { guard: Some(_) })
            );
            let verification = if matches!(outcome.status, ToolStatus::Ok)
                && let Some((repository, request)) = pr_creation.as_ref()
            {
                let (host, owner, repo) = repository;
                let result_numbers = pr_numbers_in_value(&outcome.result, host, owner, repo);
                let accepted = matches!(request, PullRequestCreationRequest::Confirmed)
                    || (matches!(request, PullRequestCreationRequest::Unresolved)
                        && !result_numbers.is_empty());
                if accepted {
                    // A background shell can transfer its write guard to the
                    // job registry before returning. Such a call has not yet
                    // reached a safe creator boundary, so do not attest it.
                    let evidence = if retains_creation_lane {
                        Self::capture_session_pr_head(session).await
                    } else {
                        None
                    };
                    if evidence.is_none() {
                        tracing::warn!(
                            session_id = session.id,
                            call_id,
                            "cannot capture immutable PR ownership evidence"
                        );
                    }
                    Some(Self::session_pr_verification_intents(
                        session,
                        repository.clone(),
                        result_numbers,
                        Vec::new(),
                        evidence,
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            drop(permit);
            (
                outcome,
                Some(monotonic_elapsed_ms(execution_started)),
                verification,
            )
        } else {
            (ToolResult::error("tool call cancelled"), None, None)
        };
        // Peel vision content ("_images") out of the result: megabytes of
        // base64 must not land in the event log or the text transcript —
        // it becomes native image input on the tool-result message instead.
        let images = take_tool_images(&mut outcome.result);
        let todos = self.persist_todos_from_result(
            &thread.id,
            &call.name,
            outcome.status,
            &outcome.result,
            Some(&call.arguments),
        )?;
        let model_result = outcome.result.to_string();
        let mut completion_events = vec![Event::ToolCompleted {
            call_id,
            status: outcome.status,
            result: outcome.result,
            execution_duration_ms,
        }];
        if let Some(todos) = todos {
            completion_events.push(Event::TodosUpdated { todos });
        }
        if let Some(intents) = verification {
            self.store
                .append_events_with_session_pr_verification_intents(
                    scope,
                    completion_events,
                    intents.clone(),
                )
                .await?;
            if !intents.is_empty() {
                self.session_pr_verification_wake.notify_one();
            }
        } else {
            self.store
                .append_events_async(scope, completion_events)
                .await?;
        }
        Ok((model_result, images))
    }

    /// The spawn tool family: `spawn_thread` starts a child agent on a new
    /// thread in the caller's session, `spawn_session` starts one in a fresh
    /// worktree session branched from the caller's branch, and
    /// `spawn_output` reports (and optionally waits for) a child's result.
    /// Guardrails: delegation depth and active tree size are bounded, at most
    /// `MAX_CONCURRENT_CHILDREN` direct children run per parent, children
    /// inherit the parent's permission mode, and read-only parents cannot
    /// escalate a child into a writing mode.
    async fn handle_spawn_tool(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        mode: &AgentPersona,
        name: &str,
        args: &serde_json::Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<serde_json::Value> {
        if name == "spawn_output" {
            let child_id = args
                .get("thread_id")
                .and_then(serde_json::Value::as_str)
                .context("thread_id is required")?;
            // Only the spawner may collect: child output can hold anything
            // the child read, so it stays within the parent's thread.
            if self.store.spawn_parent(child_id)?.as_deref() != Some(thread.id.as_str()) {
                bail!("thread {child_id} is not a child of this thread");
            }
            let wait_ms = args
                .get("wait_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                .min(180_000);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
            loop {
                let status = self.spawn_status(child_id)?;
                let running = status["status"] == "running";
                if !running || std::time::Instant::now() >= deadline {
                    return Ok(status);
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("spawn_output wait cancelled"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
                }
            }
        }

        let (root_thread_id, depth) = self.subagent_root_and_depth(&thread.id)?;
        if depth >= MAX_SUBAGENT_DEPTH {
            bail!(
                "subagent nesting is limited to {MAX_SUBAGENT_DEPTH} levels below the root thread"
            );
        }
        let tree_lock = self.subagent_tree_lock(&root_thread_id);
        let _tree_spawn = tree_lock.lock().await;

        // Respect the mode's tool policy: a restrictive/read-only persona that
        // doesn't list the spawn tool can't create branches or child agents
        // (the specs are already filtered, but a model may still emit the
        // call — deny it here too).
        if !(mode.allowed_tools.is_empty() || mode.allowed_tools.iter().any(|t| t == name)) {
            bail!("{name} is not permitted in {} mode", mode.id);
        }
        let children = self.store.spawned_children(&thread.id)?;
        let descendants = self.list_thread_descendants(&root_thread_id)?;
        {
            let active = self.active_threads.lock().unwrap();
            let running = children.iter().filter(|c| active.contains_key(*c)).count();
            if running >= MAX_CONCURRENT_CHILDREN {
                bail!("already {running} children running; collect some with spawn_output first");
            }
            let active_descendants = descendants
                .iter()
                .filter(|descendant| active.contains_key(&descendant.id))
                .count();
            if active_descendants >= MAX_ACTIVE_DESCENDANTS {
                bail!(
                    "the root subagent tree already has {active_descendants} active descendants; \
                     wait for some to finish before spawning more"
                );
            }
        }

        let prompt = args
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .context("prompt is required")?;
        let requested_child_mode = args
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&thread.mode);
        let available_personas = self.resolve_personas(Some(Path::new(&session.worktree_path)))?;
        let child_mode = personas::find_persona(&available_personas, requested_child_mode)
            .with_context(|| format!("unknown persona {requested_child_mode}"))?
            .id
            .clone();
        let parent_mode_id = personas::find_persona(&available_personas, &thread.mode)
            .map_or(thread.mode.as_str(), |persona| persona.id.as_str());
        // A read-only parent must not launch an agent that can do what it
        // itself cannot.
        if mode.read_only && child_mode != parent_mode_id {
            bail!("read-only personas can only spawn children in the same mode");
        }
        let child_model = args
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&thread.model)
            .to_string();
        if !child_model.contains('/') {
            bail!("model must be provider-qualified (e.g. openai/gpt-4.1-mini): {child_model}");
        }
        // Same model: the parent's option choices (thinking level, …) carry
        // over. A different model validates its own options; start clean.
        let model_options = if child_model == thread.model {
            self.store.thread_model_options(&thread.id)?
        } else {
            serde_json::Map::new()
        };
        let explicit_session_title = args
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(String::from);
        let supplied_child_name = ["name", "task_name", "title"].into_iter().find_map(|key| {
            args.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(String::from)
        });
        let generated_title = if supplied_child_name.is_none()
            || (name == "spawn_session" && explicit_session_title.is_none())
        {
            Some(self.generate_session_title(prompt).await.title)
        } else {
            None
        };
        let child_title = self
            .generate_subagent_title(
                supplied_child_name
                    .as_deref()
                    .or(generated_title.as_deref()),
                None,
            )
            .await;

        let (child_session_id, extra) = if name == "spawn_session" {
            let title = explicit_session_title
                .clone()
                .or_else(|| generated_title.clone())
                .expect("a missing child-session title is generated");
            // Base the child on the parent's latest checkpoint commit, not
            // its branch: turn checkpoints are written to hidden refs and
            // never move the session branch, so basing on the branch would
            // show the child none of the parent's work. Fall back to the
            // branch when there is no checkpoint yet.
            let base_ref = match self.store.latest_checkpoint_seq(&session.id)? {
                Some(seq) => self
                    .store
                    .checkpoint_at(&session.id, seq)?
                    .map(|c| c.commit_hash)
                    .unwrap_or_else(|| session.branch.clone()),
                None => session.branch.clone(),
            };
            let child_session = self
                .create_session(CreateSessionRequest {
                    workspace_id: session.workspace_id.clone(),
                    idempotency_key: None,
                    title: Some(title),
                    base_ref: Some(base_ref.clone()),
                    checkout_ref: None,
                    fetch_latest: true,
                })
                .await
                .map_err(|e| anyhow!(e.to_string()))?;
            let extra = serde_json::json!({
                "branch": child_session.branch,
                "based_on": base_ref,
                "worktree": child_session.worktree_path,
            });
            (child_session.id, Some(extra))
        } else {
            (session.id.clone(), None)
        };

        let child_session = if child_session_id == session.id {
            session.clone()
        } else {
            self.get_session(&child_session_id)
                .map_err(|error| anyhow!(error.to_string()))?
        };
        let kind = if name == "spawn_session" {
            "session"
        } else {
            "thread"
        };
        let child = self
            .create_spawned_thread_for_session(
                child_session,
                CreateThreadRequest {
                    session_id: child_session_id.clone(),
                    title: child_title,
                    mode: Some(child_mode),
                    model: Some(child_model),
                    model_options,
                    permission_mode: Some(thread.permission_mode),
                },
                &thread.id,
                kind,
            )
            .map_err(|e| anyhow!(e.to_string()))?;
        self.send_message_with_tools(&child.id, prompt.to_string(), Vec::new(), true, true)
            .map_err(|e| anyhow!(e.to_string()))?;

        let mut result = serde_json::json!({
            "thread_id": child.id,
            "session_id": child_session_id,
            "prompt": prompt,
            "model": child.model,
            "note": "child agent started; check on it with spawn_output",
        });
        if let Some(extra) = extra {
            for (k, v) in extra.as_object().unwrap() {
                result[k] = v.clone();
            }
        }
        Ok(result)
    }

    /// A child agent's status, folded from its event log: running (its
    /// dispatcher is live), failed (last turn errored), completed (ran and
    /// idle), or pending (never ran). Includes the latest assistant message
    /// and aggregate subtree token usage so the parent sees what nested
    /// delegation cost. An active descendant keeps the collected child in
    /// `running` state even if the child's own provider turn has returned.
    fn spawn_status(&self, thread_id: &str) -> Result<serde_json::Value> {
        let descendants = self.list_thread_descendants(thread_id)?;
        let (running, active_descendants) = {
            let active = self.active_threads.lock().unwrap();
            let active_descendants = descendants
                .iter()
                .filter(|descendant| active.contains_key(&descendant.id))
                .count();
            (
                active.contains_key(thread_id) || active_descendants > 0,
                active_descendants,
            )
        };
        let mut last_message = String::new();
        let mut completed_turns = 0u64;
        let mut failure: Option<String> = None;
        for envelope in self
            .store
            .events_after(&Scope::Thread(thread_id.to_string()), 0)?
        {
            match envelope.event {
                Event::AssistantMessage { content, .. } => last_message = content,
                Event::TurnCompleted { .. } => {
                    completed_turns += 1;
                    failure = None;
                }
                Event::TurnFailed { error, .. } => failure = Some(error),
                _ => {}
            }
        }
        let failed_descendants = self.store.failed_spawned_descendants(thread_id)?;
        if failure.is_none()
            && let Some(descendant) = failed_descendants.first()
        {
            failure = Some(format!("descendant thread {descendant} failed"));
        }
        let status = if running {
            "running"
        } else if failure.is_some() {
            "failed"
        } else if completed_turns > 0 {
            "completed"
        } else {
            "pending"
        };
        let usage = self.store.spawned_subtree_usage(thread_id)?;
        let mut out = serde_json::json!({
            "thread_id": thread_id,
            "status": status,
            "turns": completed_turns,
            "last_message": last_message,
            "descendants": descendants.len(),
            "active_descendants": active_descendants,
            "failed_descendants": failed_descendants,
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cost_usd": usage.cost_usd,
            },
        });
        if let Some(error) = failure {
            out["error"] = serde_json::json!(error);
        }
        Ok(out)
    }

    /// The engine-served `search_transcript` tool: recover details that
    /// compaction summarized away or a handoff digest elided. Query mode
    /// returns turn-stamped snippets from the stored event log (user and
    /// assistant messages plus tool results — already image-stripped and
    /// bounded); turn mode reads one turn's messages in full. Scoped to the
    /// current thread by default, opt-in to the session or workspace —
    /// never across workspaces.
    fn handle_search_transcript(
        &self,
        session: &Session,
        thread: &Thread,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        const MAX_MATCHES: usize = 20;
        const SNIPPET_RADIUS: usize = 120;
        const TURN_ITEM_CAP: usize = 2_000;

        // Turn mode: read one turn in full (found via a prior search).
        if let Some(turn) = args.get("turn").and_then(serde_json::Value::as_u64) {
            let target = args
                .get("thread_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&thread.id);
            let t = self
                .get_thread(target)
                .map_err(|e| anyhow!(e.to_string()))?;
            let s = self
                .get_session(&t.session_id)
                .map_err(|e| anyhow!(e.to_string()))?;
            if s.workspace_id != session.workspace_id {
                bail!("thread {target} is outside this workspace");
            }
            let mut calls: std::collections::HashMap<String, u64> = Default::default();
            let mut messages = Vec::new();
            for env in self
                .store
                .events_after(&Scope::Thread(target.to_string()), 0)?
            {
                let item = match env.event {
                    Event::UserMessage {
                        turn: t, content, ..
                    } if t == turn => {
                        serde_json::json!({"role": "user", "content": cap_chars(&content, TURN_ITEM_CAP)})
                    }
                    Event::AssistantMessage { turn: t, content } if t == turn => {
                        serde_json::json!({"role": "assistant", "content": cap_chars(&content, TURN_ITEM_CAP)})
                    }
                    Event::ToolRequested {
                        turn: t,
                        call_id,
                        tool,
                        args,
                        ..
                    } => {
                        if t != turn {
                            continue;
                        }
                        calls.insert(call_id, t);
                        serde_json::json!({"role": "tool_call", "tool": tool,
                            "args": cap_chars(&args.to_string(), TURN_ITEM_CAP)})
                    }
                    Event::ToolCompleted {
                        call_id, result, ..
                    } if calls.contains_key(&call_id) => {
                        serde_json::json!({"role": "tool_result",
                            "content": cap_chars(&result.to_string(), TURN_ITEM_CAP)})
                    }
                    Event::TurnFailed { turn: t, error } if t == turn => {
                        serde_json::json!({"role": "error", "content": error})
                    }
                    _ => continue,
                };
                messages.push(item);
            }
            if messages.is_empty() {
                bail!("no messages for turn {turn} of thread {target}");
            }
            return Ok(serde_json::json!({
                "thread_id": target,
                "turn": turn,
                "messages": messages,
            }));
        }

        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .context("query is required (or pass turn to read one turn in full)")?;
        let needle = query.to_lowercase();
        let scope = args
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("thread");
        let thread_ids: Vec<String> = match scope {
            "thread" => vec![thread.id.clone()],
            "session" => self
                .store
                .list_threads(&session.id)?
                .into_iter()
                .map(|t| t.id)
                .collect(),
            "workspace" => {
                let mut ids = Vec::new();
                for s in self.store.list_sessions(Some(&session.workspace_id))? {
                    ids.extend(self.store.list_threads(&s.id)?.into_iter().map(|t| t.id));
                }
                ids
            }
            other => bail!("unknown scope: {other} (thread | session | workspace)"),
        };

        let mut matches = Vec::new();
        let mut truncated = false;
        'threads: for tid in &thread_ids {
            let mut calls: std::collections::HashMap<String, u64> = Default::default();
            for env in self.store.events_after(&Scope::Thread(tid.clone()), 0)? {
                let (turn, role, text) = match &env.event {
                    Event::UserMessage { turn, content, .. } => (*turn, "user", content.clone()),
                    Event::AssistantMessage { turn, content } => {
                        (*turn, "assistant", content.clone())
                    }
                    Event::ToolRequested { turn, call_id, .. } => {
                        calls.insert(call_id.clone(), *turn);
                        continue;
                    }
                    Event::ToolCompleted {
                        call_id, result, ..
                    } => {
                        let Some(turn) = calls.get(call_id) else {
                            continue;
                        };
                        (*turn, "tool", result.to_string())
                    }
                    _ => continue,
                };
                let Some(at) = text.to_lowercase().find(&needle) else {
                    continue;
                };
                if matches.len() >= MAX_MATCHES {
                    truncated = true;
                    break 'threads;
                }
                let start = floor_char_boundary(&text, at.saturating_sub(SNIPPET_RADIUS));
                let end =
                    ceil_char_boundary(&text, (at + needle.len() + SNIPPET_RADIUS).min(text.len()));
                let mut snippet = String::new();
                if start > 0 {
                    snippet.push('…');
                }
                snippet.push_str(&text[start..end]);
                if end < text.len() {
                    snippet.push('…');
                }
                matches.push(serde_json::json!({
                    "thread_id": tid,
                    "turn": turn,
                    "role": role,
                    "ts": env.ts.to_rfc3339(),
                    "snippet": snippet,
                }));
            }
        }
        Ok(serde_json::json!({
            "query": query,
            "scope": scope,
            "matches": matches,
            "truncated": truncated,
            "hint": "pass {thread_id, turn} to read a matched turn in full",
        }))
    }

    async fn maybe_checkpoint(
        &self,
        session: &Session,
        thread: &Thread,
        turn: u64,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Option<String>> {
        let execution_lock = self.tool_execution_lock(&session.id);
        let _mutation_guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(None),
            guard = execution_lock.write_owned() => guard,
        };
        let worktree = PathBuf::from(&session.worktree_path);
        let dirty = {
            let wt = worktree.clone();
            tokio::task::spawn_blocking(move || git::has_changes(&wt)).await??
        };
        if !dirty {
            return Ok(None);
        }
        let seq = match retry_checkpoint_sqlite(
            cancel,
            "reading the next checkpoint sequence",
            &CHECKPOINT_SQLITE_RETRY_DELAYS,
            || self.store.next_checkpoint_seq(&session.id),
        )
        .await
        {
            Ok(Some(seq)) => seq,
            Ok(None) => return Ok(None),
            Err(error) if is_transient_sqlite_contention(&error) => {
                tracing::warn!(
                    session_id = %session.id,
                    thread_id = %thread.id,
                    turn,
                    error = %error,
                    "skipping checkpoint after repeated SQLite contention"
                );
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let message = format!("trouve: turn {turn} of {}", thread.id);
        let checkpoint_id = new_id("cp");
        let commit = self
            .executor
            .checkpoint_worktree(&worktree, &session.id, &checkpoint_id, &message)
            .await
            .map_err(anyhow::Error::msg)?;
        let row = CheckpointRow {
            id: checkpoint_id.clone(),
            session_id: session.id.clone(),
            thread_id: Some(thread.id.clone()),
            turn,
            seq,
            commit_hash: commit.clone(),
        };
        let checkpoint_event = Event::CheckpointCreated {
            checkpoint_id: checkpoint_id.clone(),
            thread_id: thread.id.clone(),
            turn,
            commit,
        };
        match retry_checkpoint_sqlite(
            cancel,
            "persisting the checkpoint and lifecycle event",
            &CHECKPOINT_SQLITE_RETRY_DELAYS,
            || {
                self.store.append_checkpoint_with_event(
                    &row,
                    Scope::Session(session.id.clone()),
                    checkpoint_event.clone(),
                )
            },
        )
        .await
        {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.executor
                    .rollback_checkpoint_worktree_ref(
                        &worktree,
                        &session.id,
                        &checkpoint_id,
                        &row.commit_hash,
                    )
                    .await
                    .map_err(anyhow::Error::msg)?;
                return Ok(None);
            }
            Err(error) if is_transient_sqlite_contention(&error) => {
                self.executor
                    .rollback_checkpoint_worktree_ref(
                        &worktree,
                        &session.id,
                        &checkpoint_id,
                        &row.commit_hash,
                    )
                    .await
                    .map_err(anyhow::Error::msg)?;
                tracing::warn!(
                    session_id = %session.id,
                    thread_id = %thread.id,
                    turn,
                    error = %error,
                    "checkpoint remained locked after bounded retries; completing the turn without an undo checkpoint"
                );
                return Ok(None);
            }
            Err(error) => {
                if let Err(rollback) = self
                    .executor
                    .rollback_checkpoint_worktree_ref(
                        &worktree,
                        &session.id,
                        &checkpoint_id,
                        &row.commit_hash,
                    )
                    .await
                {
                    return Err(error.context(format!(
                        "checkpoint persistence failed and its Git anchor could not be restored: {rollback:#}"
                    )));
                }
                return Err(error);
            }
        }
        let live_checkpoint_ids = self.store.checkpoint_ids(&session.id)?;
        if let Err(error) = self
            .executor
            .reconcile_checkpoint_worktree_refs(&worktree, &session.id, &live_checkpoint_ids)
            .await
        {
            tracing::warn!(
                session_id = %session.id,
                %error,
                "failed to reconcile checkpoint refs after persistence"
            );
        }
        Ok(Some(checkpoint_id))
    }
}

fn is_transient_sqlite_contention(error: &anyhow::Error) -> bool {
    sqlite_error_code(error).is_some_and(|code| {
        matches!(
            code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    })
}

fn is_immediate_sqlite_lock(error: &anyhow::Error) -> bool {
    sqlite_error_code(error) == Some(rusqlite::ErrorCode::DatabaseLocked)
}

fn sqlite_error_code(error: &anyhow::Error) -> Option<rusqlite::ErrorCode> {
    error
        .chain()
        .find_map(|cause| {
            cause
                .downcast_ref::<rusqlite::Error>()
                .and_then(|error| match error {
                    rusqlite::Error::SqliteFailure(details, _) => Some(details.code),
                    _ => None,
                })
        })
        .or_else(|| crate::store::event_writer_sqlite_error_code(error))
}

/// Retry one short synchronous checkpoint-store operation without retaining a
/// connection or mutex guard across the delay. `None` means cancellation won
/// while waiting; the caller can then skip optional checkpoint bookkeeping.
async fn retry_checkpoint_sqlite<T>(
    cancel: &tokio_util::sync::CancellationToken,
    operation: &'static str,
    delays: &[Duration],
    mut call: impl FnMut() -> Result<T>,
) -> Result<Option<T>> {
    let mut retry = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        match call() {
            Ok(value) => return Ok(Some(value)),
            // `busy_timeout` already waits for SQLITE_BUSY on every Store
            // connection. Only SQLITE_LOCKED bypasses that handler and should
            // receive this additional bounded retry schedule.
            Err(error) if is_immediate_sqlite_lock(&error) && retry < delays.len() => {
                let delay = delays[retry];
                retry += 1;
                tracing::debug!(
                    operation,
                    retry,
                    delay_ms = delay.as_millis(),
                    error = %error,
                    "retrying checkpoint SQLite contention"
                );
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Ok(None),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Annotate snippet-edit tool args (old/new string pairs, as sent by
/// Claude's Edit/MultiEdit and cursor's ACP edit calls) with the 1-based
/// line where each edit applies, resolved by locating the old text in the
/// pre-edit worktree file. Vendor agents apply the edit themselves right
/// after announcing it, so this is the one moment the position is knowable.
/// The hint rides in the args as `"_line"` — display metadata for the
/// client's diff gutter, never model input. Files that can't be read or
/// snippets that don't match (or match ambiguously) just skip the hint.
/// Index of the best file to suggest from a repo's GGUFs: prefer usable
/// quants that fit the GPU over CPU-only over too-large, then the best
/// quality/size trade-off quant (the catalog's Q4_K_M-class default),
/// then the smaller file.
fn recommend_gguf(files: &[trouve_protocol::LocalSearchFile]) -> usize {
    // Sub-3-bit quants are a last resort no matter what they fit on —
    // quality falls off a cliff below ~3 bits.
    fn junk_quant(quant: &str) -> bool {
        quant.starts_with("IQ1") || quant.starts_with("IQ2") || quant.starts_with("Q2")
    }
    fn quant_rank(quant: &str) -> usize {
        const PREF: &[&str] = &[
            "Q4_K_M", "Q4_K_S", "Q5_K_M", "IQ4_XS", "Q4_0", "Q5_K_S", "Q5_0", "Q6_K", "Q3_K_M",
            "Q8_0", "Q3_K_S", "IQ3_XS", "IQ3_M", "Q2_K", "F16", "BF16", "F32",
        ];
        PREF.iter().position(|p| *p == quant).unwrap_or(PREF.len())
    }
    fn fit_rank(fit: &str) -> usize {
        match fit {
            "gpu" => 0,
            "cpu" => 1,
            _ => 2,
        }
    }
    files
        .iter()
        .enumerate()
        .min_by_key(|(_, f)| {
            (
                junk_quant(&f.quant),
                fit_rank(&f.fit),
                quant_rank(&f.quant),
                f.size_bytes,
            )
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Append path references for attachments that can't ride natively in the
/// model input, so the agent can open them with its file tools.
/// Ceiling on the handoff digest, in characters (~6k tokens). Compaction
/// keeps most transcripts under this; anything longer loses its middle —
/// the opening (goals, often a compaction summary) and the recent tail
/// matter most.
const HISTORY_DIGEST_MAX: usize = 24_000;

/// Render stored transcript messages into a handoff preamble for a vendor
/// backend that hasn't seen them: everything, for a vendor joining a
/// thread mid-conversation (`resumed` false); just the interleaved turns
/// other models ran, for one being resumed after a model swap (`resumed`
/// true). Tool results are omitted — their effects live in the worktree,
/// which the vendor can inspect. Returns None when there is nothing to
/// hand off.
fn render_history_digest(messages: &[Message], resumed: bool) -> Option<String> {
    let mut body = String::new();
    for message in messages {
        let block = match message {
            Message::User(text) => format!("User:\n{}", text.trim()),
            Message::Assistant { content, .. } if !content.trim().is_empty() => {
                format!("Assistant:\n{}", content.trim())
            }
            Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                let names: Vec<&str> = tool_calls.iter().map(|c| c.name.as_str()).collect();
                format!("Assistant: [ran tools: {}]", names.join(", "))
            }
            _ => continue,
        };
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&block);
    }
    if body.is_empty() {
        return None;
    }
    if body.len() > HISTORY_DIGEST_MAX {
        let head = floor_char_boundary(&body, HISTORY_DIGEST_MAX / 4);
        let tail = ceil_char_boundary(&body, body.len() - (HISTORY_DIGEST_MAX - head));
        body = format!(
            "{}\n\n[... earlier conversation truncated — recover specifics with the \
             search_transcript tool ...]\n\n{}",
            &body[..head],
            &body[tail..]
        );
    }
    let header = if resumed {
        "[Handoff: since your last turn in this conversation, the turns below were \
         handled by a different assistant or model. Catch up from this digest and \
         continue seamlessly — do not greet the user or restate the history.]"
    } else {
        "[Handoff: you are continuing an existing conversation. Earlier turns may have \
         been handled by a different assistant or model; a digest of the conversation so \
         far follows. Continue seamlessly from it — do not greet the user or restate the \
         history.]"
    };
    Some(format!(
        "{header}\n\n{body}\n\n[End of digest. The user's current message follows.]"
    ))
}

/// Truncate to at most `max` bytes on a char boundary, marking the cut.
fn cap_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = floor_char_boundary(s, max);
    format!("{}… [truncated]", &s[..end])
}

/// Largest index `<= at` that lands on a char boundary.
fn floor_char_boundary(s: &str, mut at: usize) -> usize {
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Smallest index `>= at` that lands on a char boundary.
fn ceil_char_boundary(s: &str, mut at: usize) -> usize {
    while at < s.len() && !s.is_char_boundary(at) {
        at += 1;
    }
    at
}

fn annotate_attachments(
    content: String,
    files: &[(trouve_protocol::Attachment, PathBuf)],
) -> String {
    if files.is_empty() {
        return content;
    }
    let mut out = content;
    out.push_str(
        "\n\nThe user attached these files (read them with the file tools at the paths shown):",
    );
    for (a, path) in files {
        out.push_str(&format!("\n- {} ({}): {}", a.name, a.mime, path.display()));
    }
    out
}

/// Remove the `_images` vision payload from a tool result, leaving a small
/// summary in its place (the event log and text transcript stay lean; the
/// images travel on the provider message as native vision content).
fn take_tool_images(result: &mut serde_json::Value) -> Vec<trouve_providers::ToolImage> {
    let Some(payload) = result.as_object_mut().and_then(|o| o.remove("_images")) else {
        return Vec::new();
    };
    let images: Vec<trouve_providers::ToolImage> =
        serde_json::from_value(payload).unwrap_or_default();
    if !images.is_empty() {
        result["images"] = serde_json::json!(
            images
                .iter()
                .map(|img| {
                    serde_json::json!({
                        "mime": img.mime,
                        // Base64 expands bytes 4:3; report the real size.
                        "bytes": img.data.len() * 3 / 4,
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    images
}

/// Repository-local PR numbers found recursively in structured tool data.
fn pr_numbers_in_value(
    value: &serde_json::Value,
    host: &str,
    owner: &str,
    repo: &str,
) -> HashSet<u64> {
    let mut numbers = HashSet::new();
    match value {
        serde_json::Value::String(text) => {
            numbers.extend(crate::github::pr_numbers_in_text(text, host, owner, repo));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                numbers.extend(pr_numbers_in_value(item, host, owner, repo));
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values() {
                numbers.extend(pr_numbers_in_value(value, host, owner, repo));
            }
        }
        _ => {}
    }
    numbers
}

/// Full SHA-1 or SHA-256 commit IDs present as tokens in text.
fn git_commit_ids_in_text(text: &str) -> HashSet<String> {
    text.split(|character: char| !character.is_ascii_hexdigit())
        .filter(|token| matches!(token.len(), 40 | 64))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Full git commit IDs found recursively in structured tool data.
fn git_commit_ids_in_value(value: &serde_json::Value) -> HashSet<String> {
    let mut commits = HashSet::new();
    match value {
        serde_json::Value::String(text) => {
            commits.extend(git_commit_ids_in_text(text));
        }
        serde_json::Value::Array(items) => {
            for item in items {
                commits.extend(git_commit_ids_in_value(item));
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values() {
                commits.extend(git_commit_ids_in_value(value));
            }
        }
        _ => {}
    }
    commits
}

/// Lowercase words from tool names and arguments, with punctuation treated
/// as separators so CLI commands and snake/camel-ish tool names can be
/// recognized without depending on one provider's schema.
fn activity_words(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect()
}

fn compact_activity(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Whether a structured HTTP request mutates the expected REST collection.
fn contains_rest_mutation(
    value: &serde_json::Value,
    expected_path: &str,
    methods: &[&str],
    descendants: bool,
) -> bool {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| contains_rest_mutation(item, expected_path, methods, descendants)),
        serde_json::Value::Object(fields) => {
            let direct = fields
                .get("method")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|method| {
                    methods
                        .iter()
                        .any(|expected| method.eq_ignore_ascii_case(expected))
                })
                && fields
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|url| {
                        let path = url
                            .split(['?', '#'])
                            .next()
                            .unwrap_or(url)
                            .trim_end_matches('/')
                            .to_ascii_lowercase();
                        path.ends_with(expected_path)
                            || (descendants && path.contains(&format!("{expected_path}/")))
                    });
            direct
                || fields
                    .values()
                    .any(|item| contains_rest_mutation(item, expected_path, methods, descendants))
        }
        _ => false,
    }
}

/// Conservative repository-independent half of the REST creation predicate.
/// False positives are harmless here: the repository-aware classifier runs
/// after discovery and still requires the exact `/repos/{owner}/{repo}/pulls`
/// collection.
fn contains_pull_request_collection_post(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.iter().any(contains_pull_request_collection_post),
        serde_json::Value::Object(fields) => {
            let direct = fields
                .get("method")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
                && fields
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|url| {
                        let path = url
                            .split(['?', '#'])
                            .next()
                            .unwrap_or(url)
                            .trim_end_matches('/')
                            .to_ascii_lowercase();
                        let suffix = path
                            .split_once("/repos/")
                            .map(|(_, suffix)| suffix)
                            .or_else(|| path.strip_prefix("repos/"));
                        suffix.is_some_and(|suffix| {
                            let segments = suffix.split('/').collect::<Vec<_>>();
                            matches!(segments.as_slice(), [owner, repo, "pulls"] if !owner.is_empty() && !repo.is_empty())
                        })
                    });
            direct || fields.values().any(contains_pull_request_collection_post)
        }
        _ => false,
    }
}

fn shell_requests_pull_request_collection_post(tool: &str, args: &serde_json::Value) -> bool {
    let tool_words = activity_words(tool);
    let shell_like = ["shell", "bash", "command", "terminal", "exec", "gh"]
        .iter()
        .any(|word| tool_words.split_whitespace().any(|part| part == *word));
    shell_like
        && shell_command_values(tool, args)
            .into_iter()
            .any(|command| shell_command_posts_to_pull_collection(command, None))
}

fn shell_command_values<'a>(tool: &str, args: &'a serde_json::Value) -> Vec<&'a str> {
    fn add_value<'a>(value: &'a serde_json::Value, commands: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(command) => commands.push(command),
            serde_json::Value::Array(values) => {
                commands.extend(values.iter().filter_map(serde_json::Value::as_str))
            }
            _ => {}
        }
    }

    let mut commands = Vec::new();
    if args.is_string() {
        add_value(args, &mut commands);
        return commands;
    }
    let normalized = compact_activity(tool);
    let fields: &[&str] = if normalized == "functionsexec" || normalized.ends_with("execcommand") {
        &["cmd"]
    } else if normalized.contains("terminal") {
        &["input", "command"]
    } else if matches!(
        normalized.as_str(),
        "shell" | "bash" | "execute" | "commandexecution"
    ) || normalized.ends_with("commandexecution")
    {
        &["command"]
    } else {
        // Unknown shell-like adapters use one of these conventional command
        // fields. Never traverse descriptive or environment metadata: only
        // values that the adapter documents as executable input are evidence.
        &["command", "commands", "cmd", "input", "script"]
    };
    for field in fields {
        if let Some(value) = args.get(field) {
            add_value(value, &mut commands);
        }
    }
    commands
}

fn shell_invocations(command: &str) -> Vec<Vec<String>> {
    fn finish_word(word: &mut String, invocation: &mut Vec<String>) {
        if !word.is_empty() {
            invocation.push(std::mem::take(word));
        }
    }

    fn finish_invocation(
        word: &mut String,
        invocation: &mut Vec<String>,
        invocations: &mut Vec<Vec<String>>,
    ) {
        finish_word(word, invocation);
        if !invocation.is_empty() {
            invocations.push(std::mem::take(invocation));
        }
    }

    let mut invocations = Vec::new();
    let mut invocation = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            word.push(character);
            escaped = false;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else if character == '\\' && active_quote == '"' {
                escaped = true;
            } else {
                word.push(character);
            }
            continue;
        }
        match character {
            '\\' => escaped = true,
            '\'' | '"' => quote = Some(character),
            '\n' | ';' | '|' | '&' => {
                finish_invocation(&mut word, &mut invocation, &mut invocations);
                if characters.peek().is_some_and(|next| *next == character) {
                    characters.next();
                }
            }
            character if character.is_whitespace() => finish_word(&mut word, &mut invocation),
            _ => word.push(character),
        }
    }
    finish_invocation(&mut word, &mut invocation, &mut invocations);
    invocations
}

fn shell_word_is_command(word: &str, expected: &str) -> bool {
    word.trim_matches(|character| matches!(character, '(' | ')' | '{' | '}'))
        .rsplit('/')
        .next()
        .is_some_and(|name| name == expected)
}

fn shell_executable_index(tokens: &[String]) -> Option<usize> {
    fn is_assignment(token: &str) -> bool {
        token.split_once('=').is_some_and(|(name, _)| {
            let mut characters = name.chars();
            characters
                .next()
                .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
                && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
        })
    }

    fn consume_options(
        tokens: &[String],
        mut index: usize,
        flags: &[&str],
        value_options: &[&str],
    ) -> Option<usize> {
        while let Some(token) = tokens.get(index) {
            if token == "--" {
                return Some(index + 1);
            }
            if flags.contains(&token.as_str()) {
                index += 1;
                continue;
            }
            if value_options.contains(&token.as_str()) {
                index = index.checked_add(2)?;
                if index > tokens.len() {
                    return None;
                }
                continue;
            }
            if value_options.iter().any(|option| {
                option.starts_with("--") && token.starts_with(&format!("{option}="))
                    || option.len() == 2 && token.starts_with(option) && token.len() > 2
            }) {
                index += 1;
                continue;
            }
            return (!token.starts_with('-')).then_some(index);
        }
        None
    }

    fn after_launch_prefix(tokens: &[String], index: usize, prefix: &str) -> Option<usize> {
        let arguments = index + 1;
        match prefix {
            "env" => consume_options(
                tokens,
                arguments,
                &[
                    "-i",
                    "--ignore-environment",
                    "-0",
                    "--null",
                    "-v",
                    "--debug",
                ],
                &["-u", "--unset", "-C", "--chdir"],
            ),
            "sudo" => consume_options(
                tokens,
                arguments,
                &["-A", "-b", "-E", "-H", "-k", "-n", "-S"],
                &[
                    "-C",
                    "--close-from",
                    "-D",
                    "--chdir",
                    "-g",
                    "--group",
                    "-h",
                    "--host",
                    "-p",
                    "--prompt",
                    "-R",
                    "--chroot",
                    "-r",
                    "--role",
                    "-t",
                    "--type",
                    "-T",
                    "--command-timeout",
                    "-u",
                    "--user",
                ],
            ),
            "command" => consume_options(tokens, arguments, &["-p"], &[]),
            "nohup" => consume_options(tokens, arguments, &[], &[]),
            "exec" => consume_options(tokens, arguments, &["-c", "-l"], &["-a"]),
            "nice" => {
                let legacy_adjustment = tokens.get(arguments).is_some_and(|token| {
                    token
                        .strip_prefix('-')
                        .filter(|adjustment| !adjustment.is_empty() && !adjustment.starts_with('-'))
                        .is_some_and(|adjustment| adjustment.parse::<u32>().is_ok())
                });
                if legacy_adjustment {
                    return tokens.get(arguments + 1).map(|_| arguments + 1);
                }
                let next = consume_options(tokens, arguments, &[], &["-n", "--adjustment"])?;
                Some(next)
            }
            "stdbuf" => consume_options(
                tokens,
                arguments,
                &[],
                &["-i", "--input", "-o", "--output", "-e", "--error"],
            ),
            "timeout" => {
                let duration = consume_options(
                    tokens,
                    arguments,
                    &["--foreground", "--preserve-status", "-v", "--verbose"],
                    &["-k", "--kill-after", "-s", "--signal"],
                )?;
                tokens.get(duration + 1).map(|_| duration + 1)
            }
            _ => None,
        }
    }

    let mut index = 0;
    let mut assignments_allowed = true;
    loop {
        if assignments_allowed {
            while tokens.get(index).is_some_and(|token| is_assignment(token)) {
                index += 1;
            }
        }
        let token = tokens.get(index)?;
        let launch_prefix = [
            "env", "sudo", "command", "nohup", "exec", "timeout", "nice", "stdbuf",
        ]
        .iter()
        .find(|expected| shell_word_is_command(token, expected));
        let Some(prefix) = launch_prefix else {
            return Some(index);
        };
        index = after_launch_prefix(tokens, index, prefix)?;
        assignments_allowed = matches!(*prefix, "env" | "sudo");
        if index >= tokens.len() {
            return None;
        }
    }
}

fn shell_command_string(tokens: &[String], executable_index: usize) -> Option<&str> {
    let mut index = executable_index + 1;
    while let Some(flag) = tokens.get(index) {
        let letters = flag
            .strip_prefix('-')
            .filter(|letters| !letters.is_empty())?;
        if !letters
            .chars()
            .all(|letter| matches!(letter, 'c' | 'e' | 'i' | 'l' | 'u' | 'x'))
        {
            return None;
        }
        if letters.contains('c') {
            return tokens.get(index + 1).map(String::as_str);
        }
        index += 1;
    }
    None
}

fn pull_collection_path_matches(token: &str, expected_path: Option<&str>) -> bool {
    let path = token
        .trim_matches(|character| matches!(character, '(' | ')' | '{' | '}' | ','))
        .split(['?', '#'])
        .next()
        .unwrap_or(token)
        .trim_end_matches('/')
        .to_ascii_lowercase();
    if let Some(expected_path) = expected_path {
        let expected_path = expected_path.trim_start_matches('/');
        return path == expected_path || path.ends_with(&format!("/{expected_path}"));
    }
    let suffix = path
        .split_once("/repos/")
        .map(|(_, suffix)| suffix)
        .or_else(|| path.strip_prefix("repos/"));
    suffix.is_some_and(|suffix| {
        let segments = suffix.split('/').collect::<Vec<_>>();
        matches!(segments.as_slice(), [owner, repo, "pulls"] if !owner.is_empty() && !repo.is_empty())
    })
}

fn explicit_shell_method(tokens: &[String], long_option: &str) -> Option<bool> {
    let mut explicit_method = None;
    let long_prefix = format!("{long_option}=");
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let method = if token == "-X" || token == long_option {
            index += 1;
            tokens.get(index).map(String::as_str)
        } else if let Some(method) = token.strip_prefix(&long_prefix) {
            Some(method)
        } else {
            token.strip_prefix("-X").filter(|method| !method.is_empty())
        };
        if let Some(method) = method {
            explicit_method = Some(method.eq_ignore_ascii_case("POST"));
        }
        index += 1;
    }
    explicit_method
}

fn gh_api_invocation_is_post(tokens: &[String]) -> bool {
    if let Some(is_post) = explicit_shell_method(tokens, "--method") {
        return is_post;
    }
    tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "-f" | "-F" | "--field" | "--raw-field" | "--input"
        ) || token.starts_with("--field=")
            || token.starts_with("--raw-field=")
            || token.starts_with("--input=")
            || (token.starts_with("-f") && token.len() > 2)
            || (token.starts_with("-F") && token.len() > 2)
    })
}

fn curl_invocation_is_post(tokens: &[String]) -> bool {
    if let Some(is_post) = explicit_shell_method(tokens, "--request") {
        return is_post;
    }
    tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "-d" | "-F"
                | "--data"
                | "--data-ascii"
                | "--data-binary"
                | "--data-raw"
                | "--data-urlencode"
                | "--form"
                | "--form-string"
                | "--json"
        ) || token.starts_with("--data=")
            || token.starts_with("--data-ascii=")
            || token.starts_with("--data-binary=")
            || token.starts_with("--data-raw=")
            || token.starts_with("--data-urlencode=")
            || token.starts_with("--form=")
            || token.starts_with("--form-string=")
            || token.starts_with("--json=")
            || (token.starts_with("-d") && token.len() > 2)
            || (token.starts_with("-F") && token.len() > 2)
    })
}

fn shell_command_posts_to_pull_collection(command: &str, expected_path: Option<&str>) -> bool {
    fn inspect(command: &str, expected_path: Option<&str>, depth: usize) -> bool {
        if depth > 4 {
            return false;
        }
        shell_invocations(command).into_iter().any(|tokens| {
            let Some(executable_index) = shell_executable_index(&tokens) else {
                return false;
            };
            let executable = &tokens[executable_index];
            let shell_wrapper = ["bash", "sh", "zsh", "dash", "ksh"]
                .iter()
                .any(|expected| shell_word_is_command(executable, expected));
            if shell_wrapper
                && let Some(nested) = shell_command_string(&tokens, executable_index)
                && inspect(nested, expected_path, depth + 1)
            {
                return true;
            }
            if !tokens
                .iter()
                .any(|token| pull_collection_path_matches(token, expected_path))
            {
                return false;
            }
            if shell_word_is_command(executable, "gh")
                && tokens
                    .get(executable_index + 1)
                    .is_some_and(|arg| arg == "api")
            {
                return gh_api_invocation_is_post(&tokens[executable_index + 2..]);
            }
            if shell_word_is_command(executable, "curl") {
                return curl_invocation_is_post(&tokens[executable_index + 1..]);
            }
            false
        })
    }

    inspect(command, expected_path, 0)
}

fn is_activity_tool_wrapper(tool: &str) -> bool {
    matches!(
        compact_activity(tool).as_str(),
        "mcptoolcall" | "dynamictoolcall"
    )
}

/// Generic provider wrappers are only authoritative when they came through
/// the authenticated connector bridge. Otherwise a backend could self-report
/// a nested GitHub creator and authorize an unrelated PR.
fn trusted_activity_tool_wrapper(args: &serde_json::Value) -> bool {
    ["server", "serverName", "mcpServer", "mcpServerName"]
        .iter()
        .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
        .is_some_and(|server| server.eq_ignore_ascii_case("codex_apps"))
}

fn trusted_activity_tool_name(tool: &str) -> bool {
    let normalized = compact_activity(tool);
    normalized.starts_with("github")
        || matches!(
            normalized.as_str(),
            "apigraphql" | "apirequest" | "httprequest"
        )
}

/// Cheap classifier-owned superset used before repository discovery. Every
/// Confirmed or Unresolved call must pass this gate; false positives only pay
/// the one-time repository lookup and are rejected by the full classifier.
fn could_request_pull_request_creation(tool: &str, args: &serde_json::Value) -> bool {
    if is_activity_tool_wrapper(tool) {
        if !trusted_activity_tool_wrapper(args) {
            return false;
        }
        let Some(nested_tool) = ["tool", "toolName", "name"]
            .iter()
            .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
        else {
            return false;
        };
        let nested_compact = compact_activity(nested_tool);
        if nested_compact.contains("createpullrequest") || nested_compact.ends_with("createpr") {
            return trusted_activity_tool_name(nested_tool);
        }
        return effective_activity_tool_call(tool, args).is_some_and(
            |(nested_tool, nested_args)| {
                could_request_pull_request_creation(nested_tool, nested_args.as_ref())
            },
        );
    }

    let args_text = args.to_string();
    let args_words = activity_words(&args_text);
    let args_compact = compact_activity(&args_text);
    if (args_words.split_whitespace().any(|word| word == "mutation")
        && args_compact.contains("createpullrequest"))
        || contains_pull_request_collection_post(args)
        || shell_requests_pull_request_collection_post(tool, args)
    {
        return true;
    }

    let tool_compact = compact_activity(tool);
    if tool_compact.contains("createpullrequest") || tool_compact.ends_with("createpr") {
        return true;
    }
    has_structured_pull_request_creation_operation(args)
        || args_words.contains("gh pr create")
        || args_words.contains("create pull request")
}

/// Resolve provider-owned generic tool wrappers to the operation they carry.
fn effective_activity_tool_call<'a>(
    tool: &'a str,
    args: &'a serde_json::Value,
) -> Option<(&'a str, std::borrow::Cow<'a, serde_json::Value>)> {
    if !is_activity_tool_wrapper(tool) {
        return Some((tool, std::borrow::Cow::Borrowed(args)));
    }
    if !trusted_activity_tool_wrapper(args) {
        return None;
    }
    let nested_tool = ["tool", "toolName", "name"]
        .iter()
        .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))?;
    if !trusted_activity_tool_name(nested_tool) {
        return None;
    }
    let nested_args = match args.get("arguments")? {
        value if value.is_object() => std::borrow::Cow::Borrowed(value),
        serde_json::Value::String(encoded) => {
            let parsed = serde_json::from_str::<serde_json::Value>(encoded).ok()?;
            if !parsed.is_object() {
                return None;
            }
            std::borrow::Cow::Owned(parsed)
        }
        _ => return None,
    };
    Some((nested_tool, nested_args))
}

/// Reject explicit repository identities that do not describe the session
/// repository. Calls without structured repository fields still need the
/// command-, GraphQL-, or REST-specific checks below.
fn structured_repository_matches(args: &serde_json::Value, owner: &str, repo: &str) -> bool {
    let expected_full_name = format!("{owner}/{repo}");
    for key in ["repository_full_name", "repo_full_name"] {
        if let Some(actual) = args.get(key).and_then(serde_json::Value::as_str)
            && !actual
                .trim_matches('/')
                .eq_ignore_ascii_case(&expected_full_name)
        {
            return false;
        }
    }

    if let Some(actual_owner) = args.get("owner").and_then(serde_json::Value::as_str)
        && !actual_owner.eq_ignore_ascii_case(owner)
    {
        return false;
    }
    if let Some(actual_repo) = args.get("repo").and_then(serde_json::Value::as_str) {
        let normalized_repo = actual_repo.trim_matches('/');
        let matches = if normalized_repo.contains('/') {
            normalized_repo.eq_ignore_ascii_case(&expected_full_name)
        } else {
            normalized_repo.eq_ignore_ascii_case(repo)
        };
        if !matches {
            return false;
        }
    }
    if let Some(actual_repository) = args.get("repository").and_then(serde_json::Value::as_str) {
        let normalized_repository = actual_repository.trim_matches('/');
        let matches = if normalized_repository.contains('/') {
            normalized_repository.eq_ignore_ascii_case(&expected_full_name)
        } else {
            args.get("owner")
                .and_then(serde_json::Value::as_str)
                .is_some()
                && normalized_repository.eq_ignore_ascii_case(repo)
        };
        if !matches {
            return false;
        }
    }

    true
}

fn structured_repository_identifies(args: &serde_json::Value, owner: &str, repo: &str) -> bool {
    if !structured_repository_matches(args, owner, repo) {
        return false;
    }
    let full_name = ["repository_full_name", "repo_full_name"]
        .iter()
        .any(|key| args.get(key).and_then(serde_json::Value::as_str).is_some());
    let owner_present = args
        .get("owner")
        .and_then(serde_json::Value::as_str)
        .is_some();
    let repo_value = args.get("repo").and_then(serde_json::Value::as_str);
    let repository_value = args.get("repository").and_then(serde_json::Value::as_str);
    full_name
        || repo_value.is_some_and(|value| value.trim_matches('/').contains('/'))
        || repository_value.is_some_and(|value| value.trim_matches('/').contains('/'))
        || (owner_present && (repo_value.is_some() || repository_value.is_some()))
}

fn has_structured_pull_request_creation_operation(args: &serde_json::Value) -> bool {
    ["operation", "action"].iter().any(|key| {
        args.get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| {
                let operation = compact_activity(value);
                operation == "createpullrequest" || operation == "createpr"
            })
    })
}

fn monotonic_elapsed_ms(started: std::time::Instant) -> u64 {
    // UI durations are integral milliseconds. Round a real sub-millisecond
    // execution up instead of presenting the misleading "0ms" placeholder.
    u64::try_from(started.elapsed().as_millis().max(1)).unwrap_or(u64::MAX)
}

fn requests_pull_request_creation(
    tool: &str,
    args: &serde_json::Value,
    owner: &str,
    repo: &str,
) -> bool {
    if is_activity_tool_wrapper(tool)
        && (!trusted_activity_tool_wrapper(args)
            || !structured_repository_matches(args, owner, repo))
    {
        return false;
    }
    let Some((tool, args)) = effective_activity_tool_call(tool, args) else {
        return false;
    };
    let args = args.as_ref();
    if !structured_repository_matches(args, owner, repo) {
        return false;
    }
    let tool_words = activity_words(tool);
    let tool_compact = compact_activity(tool);
    let args_text = args.to_string();
    let args_words = activity_words(&args_text);
    let args_compact = compact_activity(&args_text);
    let shell_like = ["shell", "bash", "command", "terminal", "exec", "gh"]
        .iter()
        .any(|word| tool_words.split_whitespace().any(|part| part == *word));
    let browser_like = ["browser", "playwright", "web", "click"]
        .iter()
        .any(|word| tool_words.split_whitespace().any(|part| part == *word));
    let graphql_mutation = args_words.split_whitespace().any(|word| word == "mutation")
        && args_compact.contains("createpullrequest");
    let rest_path = format!("/repos/{owner}/{repo}/pulls").to_ascii_lowercase();
    let shell_rest_creation = shell_like
        && shell_command_values(tool, args)
            .into_iter()
            .any(|command| shell_command_posts_to_pull_collection(command, Some(&rest_path)));
    let github_like = tool_compact == "github";
    let structured_creation_operation = github_like
        && structured_repository_identifies(args, owner, repo)
        && has_structured_pull_request_creation_operation(args);
    tool_compact.contains("createpullrequest")
        || tool_compact.ends_with("createpr")
        || structured_creation_operation
        || (shell_like && args_words.contains("gh pr create"))
        || graphql_mutation
        || (browser_like && args_words.contains("create pull request"))
        || shell_rest_creation
        || contains_rest_mutation(args, &rest_path, &["POST"], false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PullRequestCreationRequest {
    Confirmed,
    Unresolved,
    Rejected,
}

fn classify_pull_request_creation(
    tool: &str,
    args: &serde_json::Value,
    owner: &str,
    repo: &str,
) -> PullRequestCreationRequest {
    if requests_pull_request_creation(tool, args, owner, repo) {
        return PullRequestCreationRequest::Confirmed;
    }

    if !is_activity_tool_wrapper(tool)
        || !trusted_activity_tool_wrapper(args)
        || !structured_repository_matches(args, owner, repo)
    {
        return PullRequestCreationRequest::Rejected;
    }
    let Some(nested_tool) = ["tool", "toolName", "name"]
        .iter()
        .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
    else {
        return PullRequestCreationRequest::Rejected;
    };
    let nested_compact = compact_activity(nested_tool);
    let named_creator =
        nested_compact.contains("createpullrequest") || nested_compact.ends_with("createpr");
    if named_creator
        && trusted_activity_tool_name(nested_tool)
        && effective_activity_tool_call(tool, args).is_none()
    {
        PullRequestCreationRequest::Unresolved
    } else {
        PullRequestCreationRequest::Rejected
    }
}

/// A successful tool call that creates or updates a remote branch. This is
/// the evidence needed to find a PR opened later through github.com.
fn requests_remote_ref_mutation(
    tool: &str,
    args: &serde_json::Value,
    owner: &str,
    repo: &str,
) -> bool {
    let tool_words = activity_words(tool);
    let tool_compact = compact_activity(tool);
    let args_text = args.to_string();
    let args_words = activity_words(&args_text);
    let args_compact = compact_activity(&args_text);
    let shell_like = ["shell", "bash", "command", "terminal", "exec", "gh"]
        .iter()
        .any(|word| tool_words.split_whitespace().any(|part| part == *word));
    let graphql_mutation = args_words.split_whitespace().any(|word| word == "mutation")
        && (args_compact.contains("createref") || args_compact.contains("updateref"));
    let rest_path = format!("/repos/{owner}/{repo}/git/refs").to_ascii_lowercase();
    let args_lower = args_text.to_ascii_lowercase();
    let shell_rest_mutation = shell_like
        && (args_lower.contains(&rest_path)
            || args_lower.contains(rest_path.trim_start_matches('/')))
        && [" post ", " patch ", " put "]
            .iter()
            .any(|method| format!(" {args_words} ").contains(method));

    [
        "pushbranch",
        "createbranch",
        "updateref",
        "createref",
        "pushref",
    ]
    .iter()
    .any(|operation| tool_compact.contains(operation))
        || (shell_like && args_words.contains("git push"))
        || graphql_mutation
        || shell_rest_mutation
        || contains_rest_mutation(args, &rest_path, &["POST", "PATCH", "PUT"], true)
}

/// Evidence that associates PRs with a session independently of the client.
#[derive(Default)]
struct SessionPrEvidence {
    numbers: HashSet<u64>,
    recorded_numbers: HashSet<u64>,
    successful_tool_args: Vec<String>,
    commit_ids: HashSet<String>,
}

impl SessionPrEvidence {
    /// Merge evidence collected from another thread into this session.
    fn extend(&mut self, other: Self) {
        self.numbers.extend(other.numbers);
        self.recorded_numbers.extend(other.recorded_numbers);
        self.successful_tool_args.extend(other.successful_tool_args);
        self.commit_ids.extend(other.commit_ids);
    }
}

/// Collect PR references, successful branch activity, and commits from events.
fn pr_evidence_from_events(
    events: impl IntoIterator<Item = Event>,
    host: &str,
    owner: &str,
    repo: &str,
) -> SessionPrEvidence {
    let mut requested = HashMap::new();
    let mut output = HashMap::<String, String>::new();
    let mut evidence = SessionPrEvidence::default();
    for event in events {
        match event {
            Event::ToolRequested {
                call_id,
                tool,
                args,
                ..
            } => {
                requested.insert(call_id, (tool, args));
            }
            Event::ToolOutput { call_id, chunk } => {
                output.entry(call_id).or_default().push_str(&chunk);
            }
            Event::ToolCompleted {
                call_id,
                status,
                result,
                ..
            } => {
                let request = requested.remove(&call_id);
                let output = output.remove(&call_id).unwrap_or_default();
                if matches!(status, ToolStatus::Ok)
                    && let Some((tool, args)) = request
                {
                    let request = classify_pull_request_creation(&tool, &args, owner, repo);
                    let (result_numbers, output_numbers) = if matches!(
                        request,
                        PullRequestCreationRequest::Confirmed
                            | PullRequestCreationRequest::Unresolved
                    ) {
                        (
                            pr_numbers_in_value(&result, host, owner, repo),
                            crate::github::pr_numbers_in_text(&output, host, owner, repo),
                        )
                    } else {
                        (HashSet::new(), Vec::new())
                    };
                    let creates_pr = matches!(request, PullRequestCreationRequest::Confirmed)
                        || (matches!(request, PullRequestCreationRequest::Unresolved)
                            && (!result_numbers.is_empty() || !output_numbers.is_empty()));
                    let mutates_ref = requests_remote_ref_mutation(&tool, &args, owner, repo);
                    if creates_pr {
                        evidence.numbers.extend(result_numbers);
                        evidence.numbers.extend(output_numbers);
                    }
                    if creates_pr || mutates_ref {
                        evidence.commit_ids.extend(git_commit_ids_in_value(&args));
                        evidence.commit_ids.extend(git_commit_ids_in_value(&result));
                        evidence.commit_ids.extend(git_commit_ids_in_text(&output));
                        evidence.successful_tool_args.push(args.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    evidence
}

/// Make a stored transcript safe to send to a provider. A crash or restart
/// between persisting an assistant message with `tool_calls` and persisting
/// its results (tool execution can take minutes; approval waits are
/// unbounded) leaves a dangling `tool_call`, which both OpenAI and Anthropic
/// reject — permanently wedging the thread. Synthesize an "interrupted"
/// result for every tool call left unanswered, and drop empty assistant
/// messages (they serialize to an empty content block Anthropic rejects).
/// Provider call ids are transcript keys as well as UI/event identities.
/// Preserve the first non-empty id and replace missing or duplicate ids
/// before either the assistant message or concurrent execution observes
/// them.
fn normalize_tool_call_ids(calls: &mut [trouve_providers::ToolCallRequest]) {
    let mut seen = HashSet::with_capacity(calls.len());
    for call in calls {
        if call.id.trim().is_empty() || !seen.insert(call.id.clone()) {
            loop {
                let id = new_id("call");
                if seen.insert(id.clone()) {
                    call.id = id;
                    break;
                }
            }
        }
    }
}

fn sanitize_transcript(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();
    while let Some(msg) = iter.next() {
        match msg {
            Message::Assistant {
                content,
                tool_calls,
                reasoning,
            } => {
                if content.trim().is_empty() && tool_calls.is_empty() {
                    continue;
                }
                let ids: Vec<String> = tool_calls.iter().map(|c| c.id.clone()).collect();
                out.push(Message::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                });
                if ids.is_empty() {
                    continue;
                }
                // Absorb the contiguous run of results that follow, tracking
                // which call ids they answer.
                let mut answered = std::collections::HashSet::new();
                while matches!(iter.peek(), Some(Message::ToolResult { .. })) {
                    if let Some(Message::ToolResult {
                        call_id,
                        content,
                        images,
                    }) = iter.next()
                    {
                        answered.insert(call_id.clone());
                        out.push(Message::ToolResult {
                            call_id,
                            content,
                            images,
                        });
                    }
                }
                for id in ids {
                    if !answered.contains(&id) {
                        out.push(Message::ToolResult {
                            call_id: id,
                            content: "Tool call interrupted; no result was recorded.".into(),
                            images: Vec::new(),
                        });
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn annotate_edit_lines(worktree: &Path, args: &mut serde_json::Value) {
    let str_of = |v: &serde_json::Value, keys: &[&str]| {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(serde_json::Value::as_str))
            .map(str::to_string)
    };
    let Some(path) = str_of(
        args,
        &["file_path", "path", "abs_path", "target_file", "filePath"],
    ) else {
        return;
    };
    let full = if Path::new(&path).is_absolute() {
        PathBuf::from(&path)
    } else {
        worktree.join(&path)
    };
    // Only bother when there is at least one old/new snippet to place.
    let has_snippets = args.get("edits").map(|e| e.is_array()).unwrap_or(false)
        || ["old_string", "oldText", "old_text", "old_str"]
            .iter()
            .any(|k| args.get(*k).is_some());
    if !has_snippets {
        return;
    }
    let Ok(mut content) = std::fs::read_to_string(&full) else {
        return;
    };

    // Locate one snippet in `content` (must be unambiguous), then apply the
    // edit so later snippets in a MultiEdit see their predecessors' effect.
    let mut place = |edit: &mut serde_json::Value| {
        let old = str_of(edit, &["old_string", "oldText", "old_text", "old_str"]);
        let new = str_of(edit, &["new_string", "newText", "new_text", "new_str"]);
        let (Some(old), Some(new)) = (old, new) else {
            return;
        };
        if old.is_empty() || content.matches(old.as_str()).nth(1).is_some() {
            return;
        }
        let Some(pos) = content.find(old.as_str()) else {
            return;
        };
        let line = 1 + content[..pos].matches('\n').count();
        edit["_line"] = serde_json::json!(line);
        content = format!("{}{}{}", &content[..pos], new, &content[pos + old.len()..]);
    };
    match args.get_mut("edits").and_then(|v| v.as_array_mut()) {
        Some(edits) => edits.iter_mut().for_each(&mut place),
        None => place(args),
    }
}

/// Tool spec for the engine-served `ask_question` tool (native provider
/// turns and the MCP bridge expose the same schema).
pub fn ask_question_spec() -> ToolSpec {
    ToolSpec {
        name: "ask_question".into(),
        description: "Ask the user one or more multiple-choice questions and wait for their \
                      answers. Use this when you are blocked on a decision only the user can \
                      make. Each question offers your listed options plus an automatic \
                      free-form \"Other\" choice; set allow_multiple for checkbox-style \
                      questions. The user may also skip answering entirely."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Optional short title for the question form."
                },
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "Stable id; generated when omitted." },
                            "prompt": { "type": "string", "description": "The question text." },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "label": { "type": "string" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "allow_multiple": {
                                "type": "boolean",
                                "description": "Allow selecting more than one option."
                            }
                        },
                        "required": ["prompt", "options"]
                    }
                }
            },
            "required": ["questions"]
        }),
    }
}

/// Spec for the engine-served `spawn_thread` tool (child agent, same
/// session/worktree). Offered while the caller remains below the bounded
/// recursive delegation depth.
pub fn spawn_thread_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_thread".into(),
        description: "Start a child agent on a new thread in this session (same working \
                      tree). Returns the child's thread_id immediately; collect results \
                      with spawn_output. The child inherits your mode, model and \
                      permission level unless overridden. Children run concurrently with \
                      your turn. Same-session mutations are serialized by trouve, but \
                      agents share one worktree and can still make semantically conflicting \
                      changes; use spawn_session when work needs full isolation."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task for the child agent. Self-contained: the child does not see your conversation."
                },
                "name": {
                    "type": "string",
                    "description": "Optional concise name for the child. When omitted, trouve applies the configured session/thread naming model to the prompt."
                },
                "mode": {
                    "type": "string",
                    "description": "Agent persona id for the child (default: your persona). Use a read-only persona like \"plan\" for concurrent research."
                },
                "model": {
                    "type": "string",
                    "description": "Provider-qualified model for the child (default: your model)."
                }
            },
            "required": ["prompt"]
        }),
    }
}

/// Spec for the engine-served `spawn_session` tool (child agent, isolated
/// worktree).
pub fn spawn_session_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_session".into(),
        description: "Start a child agent in a NEW session with its own git worktree and \
                      branch, based on your latest checkpoint (your work up to the last \
                      completed turn — not the current turn's uncommitted changes). Fully \
                      isolated: it cannot touch your files; its work lands on its own \
                      branch for later review or merge. Returns thread_id, session_id and \
                      branch immediately; collect results with spawn_output. Use for risky \
                      experiments, best-of-N attempts, or parallel feature work."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task for the child agent. Self-contained: the child does not see your conversation."
                },
                "title": {
                    "type": "string",
                    "description": "Session title; derived from the prompt when omitted. It also contributes to the branch name when title-derived branch naming is enabled."
                },
                "name": {
                    "type": "string",
                    "description": "Optional concise name for the child thread. The session title is used when omitted; otherwise trouve applies the configured session/thread naming model to the prompt."
                },
                "mode": {
                    "type": "string",
                    "description": "Agent persona id for the child (default: your persona)."
                },
                "model": {
                    "type": "string",
                    "description": "Provider-qualified model for the child (default: your model)."
                }
            },
            "required": ["prompt"]
        }),
    }
}

/// Spec for the engine-served `spawn_output` tool (child status/result
/// collection).
pub fn spawn_output_spec() -> ToolSpec {
    ToolSpec {
        name: "spawn_output".into(),
        description: "Status and latest output of a child agent you spawned with \
                      spawn_thread or spawn_session. Returns status (pending | running \
                      | completed | failed), the child's last assistant message, turns \
                      completed, and token usage. Set wait_ms to block until the child \
                      finishes or the timeout passes."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "thread_id": {
                    "type": "string",
                    "description": "The child's thread id, as returned by spawn_thread/spawn_session."
                },
                "wait_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 180000,
                    "description": "Milliseconds to wait for the child to finish (default 0: return current status immediately)."
                }
            },
            "required": ["thread_id"]
        }),
    }
}

/// Spec for the engine-served `search_transcript` tool (recovering history
/// lost to compaction or handoff digests, and cross-thread memory).
pub fn search_transcript_spec() -> ToolSpec {
    ToolSpec {
        name: "search_transcript".into(),
        description: "Search the stored conversation history, including turns that were \
                      compacted out of your context or elided from a handoff digest. \
                      Returns turn-stamped snippets around each match (user and \
                      assistant messages plus tool results). scope defaults to this \
                      thread; \"session\" covers all threads in this session, \
                      \"workspace\" every session in this workspace. To read a matched \
                      turn in full, call again with turn (and the match's thread_id) \
                      instead of a query."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Case-insensitive text to find (exact substring match)."
                },
                "scope": {
                    "type": "string",
                    "enum": ["thread", "session", "workspace"],
                    "description": "How far to search (default: thread)."
                },
                "turn": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Read this turn's messages in full instead of searching."
                },
                "thread_id": {
                    "type": "string",
                    "description": "Thread the turn belongs to (default: this thread); from a match."
                }
            }
        }),
    }
}

/// Parse `ask_question` tool arguments into protocol questions, synthesizing
/// ids where the model omitted them.
pub fn parse_question_args(
    args: &serde_json::Value,
) -> std::result::Result<(Option<String>, Vec<trouve_protocol::Question>), String> {
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let raw = args
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or("missing questions array")?;
    let mut questions = Vec::new();
    for (qi, q) in raw.iter().enumerate() {
        let prompt = q
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("question {} has no prompt", qi + 1))?;
        let id = q
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("q{}", qi + 1));
        let mut options = Vec::new();
        for (oi, o) in q
            .get("options")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            // Accept both {id,label} objects and bare strings.
            let label = o
                .get("label")
                .and_then(|v| v.as_str())
                .or_else(|| o.as_str())
                .unwrap_or_default()
                .to_string();
            if label.trim().is_empty() {
                continue;
            }
            let oid = o
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("opt{}", oi + 1));
            options.push(trouve_protocol::QuestionOption { id: oid, label });
        }
        if options.is_empty() {
            return Err(format!("question {} has no options", qi + 1));
        }
        questions.push(trouve_protocol::Question {
            id,
            prompt: prompt.to_string(),
            options,
            allow_multiple: q
                .get("allow_multiple")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }
    if questions.is_empty() {
        return Err("questions array is empty".into());
    }
    Ok((title, questions))
}

/// Fold question answers into the JSON fed back to the model. Selected
/// options are echoed as labels (ids may have been synthesized, so labels
/// are what the model recognizes).
pub fn question_result_json(
    questions: &[trouve_protocol::Question],
    answers: Option<Vec<trouve_protocol::QuestionAnswer>>,
) -> serde_json::Value {
    let Some(answers) = answers else {
        return serde_json::json!({
            "status": "skipped",
            "message": "The user declined to answer the questions.",
        });
    };
    let items: Vec<serde_json::Value> = answers
        .iter()
        .map(|a| {
            let q = questions.iter().find(|q| q.id == a.question_id);
            let selected: Vec<String> = a
                .selected_option_ids
                .iter()
                .map(|id| {
                    q.and_then(|q| q.options.iter().find(|o| &o.id == id))
                        .map(|o| o.label.clone())
                        .unwrap_or_else(|| id.clone())
                })
                .collect();
            serde_json::json!({
                "question": q.map(|q| q.prompt.as_str()).unwrap_or(a.question_id.as_str()),
                "selected": selected,
                "other": a.other_text,
            })
        })
        .collect();
    serde_json::json!({ "status": "answered", "answers": items })
}

/// Build a provider from config. Credential resolution order: inline
/// `api_key` > `api_key_env` > secret store API key > stored OAuth tokens
/// (when `[providers.<id>.oauth]` is configured).
/// Stream one GGUF from HuggingFace to `<data_dir>/models/`, updating
/// `counter` as bytes land. Writes to a `.part` sibling and renames on
/// success so a partial download never looks complete. Returns false when
/// `cancel` was set (the partial file is deleted).
async fn download_gguf(
    data_dir: &Path,
    entry: &crate::local::ModelEntry,
    counter: &std::sync::atomic::AtomicU64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<bool> {
    use futures::TryStreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let target = crate::local::gguf_path(data_dir, entry);
    std::fs::create_dir_all(target.parent().unwrap())?;
    let part = target.with_extension("gguf.part");

    let url = crate::local::download_url(&entry.repo, &entry.file);
    let client = reqwest::Client::builder()
        .user_agent(concat!("trouve/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client.get(&url).send().await?.error_for_status()?;
    let content_length = resp.content_length();
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(&part).await?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = stream.try_next().await? {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Ok(false);
        }
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        counter.fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    file.flush().await?;
    drop(file);

    // Integrity checks before promoting the .part file: catch a truncated
    // download (connection dropped mid-stream) or a wrong file served from
    // the mutable `main` ref. Without these a partial/corrupt GGUF would be
    // renamed to final and loaded.
    let verify = |ok: bool, msg: String| -> Result<()> {
        if ok {
            Ok(())
        } else {
            let _ = std::fs::remove_file(&part);
            bail!(msg)
        }
    };
    if let Some(expected) = content_length {
        verify(
            downloaded == expected,
            format!("download truncated: got {downloaded} of {expected} bytes"),
        )?;
    }
    if entry.size_bytes > 0 {
        // Allow a small drift (the curated size can lag a re-quantization),
        // but reject anything clearly wrong.
        let expected = entry.size_bytes;
        let tolerance = expected / 100; // 1%
        let diff = downloaded.abs_diff(expected);
        verify(
            diff <= tolerance,
            format!(
                "downloaded size {downloaded} differs from the expected {expected} by more than 1%"
            ),
        )?;
    }

    std::fs::rename(&part, &target)?;
    Ok(true)
}

fn build_provider(
    id: &str,
    pc: &ProviderConfig,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
    catalog: &Arc<trouve_providers::models_dev::ModelsDevCatalog>,
) -> Result<Arc<dyn Provider>> {
    use trouve_providers::auth::{StaticToken, StoredOAuthToken, TokenSource};
    use trouve_providers::secrets::oauth_secret;

    let api_key = resolved_api_key(id, pc, secrets);
    let mut values = pc.settings.clone();
    for name in &pc.secret_names {
        if let Some(value) = secrets.get(&trouve_providers::secrets::provider_secret(id, name))? {
            values.insert(name.clone(), value);
        }
    }
    let known_preset = catalog
        .provider_presets()
        .into_iter()
        .find(|provider| provider.id == id && provider.kind == pc.kind);
    if let Some(preset) = &known_preset {
        for field in &preset.config_fields {
            if !values.contains_key(&field.id)
                && let Some(value) = field
                    .env
                    .as_ref()
                    .and_then(|name| std::env::var(name).ok())
                    .filter(|value| !value.is_empty())
            {
                values.insert(field.id.clone(), value);
            }
        }
    }
    if let Some(key) = &api_key {
        values.insert("API_KEY".into(), key.clone());
    }
    let expand = |template: &str| expand_provider_template(template, &values);
    let base_url = pc.base_url.as_deref().map(expand).transpose()?;
    let mut headers = pc
        .headers
        .iter()
        .map(|(name, value)| Ok((name.clone(), expand(value)?)))
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
    if pc.kind == "azure-openai" && !headers.contains_key("api-key") {
        headers.insert(
            "api-key".into(),
            api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("azure-openai requires an API key"))?,
        );
    }
    let query_params = pc
        .query_params
        .iter()
        .map(|(name, value)| Ok((name.clone(), expand(value)?)))
        .collect::<Result<std::collections::BTreeMap<_, _>>>()?;

    if let Some(endpoint) = &base_url {
        let parsed = reqwest::Url::parse(endpoint)
            .with_context(|| format!("provider {id} endpoint is not a valid URL"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https"),
            "provider {id} endpoint must use http or https"
        );
    }

    if pc.kind == "amazon-bedrock" {
        return Ok(Arc::new(trouve_providers::bedrock::BedrockProvider::new(
            id,
            values
                .get("AWS_REGION")
                .cloned()
                .or_else(|| std::env::var("AWS_REGION").ok()),
            values
                .get("AWS_PROFILE")
                .cloned()
                .or_else(|| std::env::var("AWS_PROFILE").ok()),
            catalog.clone(),
        )));
    }
    if pc.kind == "google-vertex" {
        let endpoint =
            base_url.ok_or_else(|| anyhow::anyhow!("google-vertex requires an endpoint"))?;
        return Ok(Arc::new(trouve_providers::vertex::VertexProvider::new(
            id,
            endpoint,
            values.get("GOOGLE_APPLICATION_CREDENTIALS").cloned(),
            catalog.clone(),
        )));
    }
    if pc.kind == "google-vertex-anthropic" {
        let endpoint = base_url
            .ok_or_else(|| anyhow::anyhow!("google-vertex-anthropic requires an endpoint"))?;
        let token = Arc::new(trouve_providers::vertex::GoogleAccessToken::new(
            values.get("GOOGLE_APPLICATION_CREDENTIALS").cloned(),
        ));
        return Ok(Arc::new(
            trouve_providers::anthropic::AnthropicProvider::new(id, Some(endpoint), token)
                .with_catalog(catalog.clone())
                .with_catalog_provider(id)
                .with_vertex_bearer(),
        ));
    }
    // Local endpoints (e.g. Ollama) don't need a key; send an empty token.
    let local = base_url.as_deref().is_some_and(is_loopback_base_url);
    let mut oauth_bearer = false;
    let token: Arc<dyn TokenSource> = match (api_key, &pc.oauth) {
        (Some(key), _) => Arc::new(StaticToken(key)),
        (None, Some(oauth)) => {
            oauth_bearer = true;
            Arc::new(StoredOAuthToken::new(
                secrets.clone(),
                oauth_secret(id),
                oauth.clone(),
            ))
        }
        (None, None) if local => Arc::new(StaticToken(String::new())),
        (None, None) => anyhow::bail!(
            "no credentials: set api_key/api_key_env, store a key with \
             `trouve auth set-key {id}`, or configure [providers.{id}.oauth]"
        ),
    };
    let bearer_auth = !pc
        .headers
        .values()
        .chain(pc.query_params.values())
        .any(|value| value.contains("${API_KEY}"));
    let known_catalog_provider = known_preset.map(|provider| provider.id);
    match pc.kind.as_str() {
        "openai-compat" => {
            let mut provider = trouve_providers::openai_compat::OpenAiCompatProvider::with_token(
                id.to_string(),
                base_url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
                token,
            )
            .with_catalog(catalog.clone())
            .with_http_options(bearer_auth, headers, query_params);
            if let Some(catalog_provider) = known_catalog_provider {
                provider = provider.with_catalog_provider(catalog_provider);
            }
            Ok(Arc::new(provider))
        }
        "azure-openai" => {
            let endpoint =
                base_url.ok_or_else(|| anyhow::anyhow!("azure-openai requires an endpoint"))?;
            let catalog_provider = known_catalog_provider.unwrap_or_else(|| "azure".into());
            Ok(Arc::new(trouve_providers::azure::AzureOpenAiProvider::new(
                id,
                endpoint,
                token,
                catalog.clone(),
                catalog_provider,
                headers,
                query_params,
            )))
        }
        "anthropic" => {
            let mut provider = trouve_providers::anthropic::AnthropicProvider::new(
                id.to_string(),
                base_url,
                token,
            )
            .with_catalog(catalog.clone())
            .with_http_options(bearer_auth, headers, query_params);
            if let Some(catalog_provider) = known_catalog_provider {
                provider = provider.with_catalog_provider(catalog_provider);
            }
            if oauth_bearer {
                provider = provider.with_oauth_bearer();
            }
            Ok(Arc::new(provider))
        }
        other => anyhow::bail!("unknown provider kind {other:?}"),
    }
}

fn resolved_api_key(
    id: &str,
    provider: &ProviderConfig,
    secrets: &Arc<dyn trouve_providers::secrets::SecretStore>,
) -> Option<String> {
    provider
        .api_key
        .clone()
        .or_else(|| {
            provider
                .api_key_env
                .as_ref()
                .and_then(|variable| std::env::var(variable).ok())
        })
        .or_else(|| {
            secrets
                .get(&trouve_providers::secrets::api_key_secret(id))
                .ok()
                .flatten()
        })
}

/// Expand only literal `${NAME}` placeholders. This deliberately does not
/// invoke a shell or implement shell parameter syntax.
fn expand_provider_template(
    template: &str,
    values: &std::collections::BTreeMap<String, String>,
) -> Result<String> {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("unterminated provider template placeholder"))?;
        let name = &rest[..end];
        anyhow::ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_'),
            "invalid provider template placeholder {name:?}"
        );
        let value = values
            .get(name)
            .cloned()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing provider setting {name}"))?;
        output.push_str(&value);
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn planned_turn_setup_lane_bounds_only_setup_admission() {
        let scheduler = TurnScheduler::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut permits = Vec::with_capacity(PLANNED_TURN_SETUP_CONCURRENCY);
        for _ in 0..PLANNED_TURN_SETUP_CONCURRENCY {
            permits.push(scheduler.acquire_planned_setup(&cancel).await.unwrap());
        }

        let blocked_cancel = tokio_util::sync::CancellationToken::new();
        let blocked = scheduler.acquire_planned_setup(&blocked_cancel);
        tokio::pin!(blocked);
        tokio::select! {
            biased;
            result = &mut blocked => panic!("setup lane admitted excess work: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        blocked_cancel.cancel();
        assert_eq!(
            blocked.await.unwrap_err().to_string(),
            "turn setup cancelled"
        );

        drop(permits.pop());
        let _replacement = scheduler.acquire_planned_setup(&cancel).await.unwrap();
    }

    fn persona_request(display_name: &str) -> trouve_protocol::UpsertPersonaRequest {
        trouve_protocol::UpsertPersonaRequest {
            display_name: display_name.into(),
            group: trouve_protocol::PersonaGroup::General,
            system_prompt: format!("Act as {display_name}."),
            allowed_tools: vec!["read_file".into()],
            read_only: true,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        }
    }

    struct RejectingPersonaDeletionExecutor {
        attempted: Arc<std::sync::atomic::AtomicBool>,
    }

    struct BlockingPersonaUpsertExecutor {
        started: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for RejectingPersonaDeletionExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            None
        }

        async fn execute(
            &self,
            _ctx: &ToolCtx,
            name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::error(format!("unknown tool: {name}"))
        }

        async fn upsert_persona_file(
            &self,
            _config_dir: &Path,
            _persona: &AgentPersona,
        ) -> Result<(), String> {
            self.attempted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Err("injected persona upsert failure".into())
        }

        async fn delete_persona_file(
            &self,
            _config_dir: &Path,
            _id: &str,
            _allow_missing: bool,
        ) -> Result<(), String> {
            self.attempted
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Err("injected persona deletion failure".into())
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for BlockingPersonaUpsertExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            None
        }

        async fn execute(
            &self,
            _ctx: &ToolCtx,
            name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::error(format!("unknown tool: {name}"))
        }

        async fn upsert_persona_file(
            &self,
            _config_dir: &Path,
            _persona: &AgentPersona,
        ) -> Result<(), String> {
            self.started.add_permits(1);
            self.release
                .acquire()
                .await
                .map_err(|error| error.to_string())?
                .forget();
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_persona_upsert_does_not_consume_pending_deletion() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store.begin_persona_deletion("custom").unwrap();
        let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let engine = Engine::new(store.clone(), tmp.path().join("data"), &Config::default())
            .with_config_dir(Some(tmp.path().join("config")))
            .with_executor(Arc::new(RejectingPersonaDeletionExecutor {
                attempted: attempted.clone(),
            }));

        let error = engine
            .upsert_persona("custom", persona_request("Custom"))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected persona upsert failure")
        );
        assert!(attempted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(store.persona_deletion_pending("custom").unwrap());
    }

    #[tokio::test]
    async fn cancelled_replacement_waiter_does_not_detach_its_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store.begin_persona_deletion("custom").unwrap();
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let engine = Arc::new(
            Engine::new(store.clone(), tmp.path().join("data"), &Config::default())
                .with_config_dir(Some(tmp.path().join("config")))
                .with_executor(Arc::new(BlockingPersonaUpsertExecutor {
                    started: started.clone(),
                    release: release.clone(),
                })),
        );
        let request = tokio::spawn({
            let engine = engine.clone();
            async move {
                engine
                    .upsert_persona("custom", persona_request("Custom"))
                    .await
            }
        });
        started.acquire().await.unwrap().forget();
        request.abort();
        release.add_permits(1);

        tokio::time::timeout(Duration::from_secs(2), async {
            while store.persona_deletion_pending("custom").unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn persona_mutations_reject_unsafe_ids_before_file_access() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let outside = config_dir.join("escape.toml");
        std::fs::write(&outside, "sentinel").unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            tmp.path().join("data"),
            &Config::default(),
        )
        .with_config_dir(Some(config_dir));

        for id in ["", "../escape", "has/slash", "has space", "question?"] {
            assert!(matches!(
                engine.upsert_persona(id, persona_request("Unsafe")).await,
                Err(EngineError::BadRequest(_))
            ));
            assert!(matches!(
                engine.delete_persona(id).await,
                Err(EngineError::BadRequest(_))
            ));
        }
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "sentinel");
    }

    #[tokio::test]
    async fn legacy_colon_reviewer_ids_remain_editable_and_deletable() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_reviewer_profile(&trouve_protocol::ReviewerProfile {
                id: "custom:legacy".into(),
                name: "Legacy".into(),
                prompt: "Review".into(),
                model: None,
                default_thinking_level: None,
                built_in: false,
            })
            .unwrap();
        let engine = Engine::new(store.clone(), tmp.path().join("data"), &Config::default())
            .with_config_dir(Some(tmp.path().join("config")));
        let policy = crate::reviewers::reviewer_as_persona(
            &store.list_custom_reviewer_profiles().unwrap()[0],
        );
        let request = trouve_protocol::UpsertPersonaRequest {
            display_name: "Updated legacy".into(),
            group: policy.group,
            system_prompt: policy.system_prompt,
            allowed_tools: policy.allowed_tools,
            read_only: policy.read_only,
            default_permission_mode: policy.default_permission_mode,
            default_model: policy.default_model,
            default_thinking_level: policy.default_thinking_level,
        };

        engine
            .upsert_persona("custom:legacy", request)
            .await
            .unwrap();
        assert_eq!(
            store.list_custom_reviewer_profiles().unwrap()[0].name,
            "Updated legacy"
        );
        engine.delete_persona("custom:legacy").await.unwrap();
        assert!(store.list_custom_reviewer_profiles().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_persona_cleans_custom_references_and_resets_system_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let workspace = tmp.path().join("workspace");
        let store = Store::open_in_memory().unwrap();
        let built_in_default = crate::reviewers::built_in_reviewers()
            .into_iter()
            .find(|reviewer| reviewer.id == "correctness")
            .unwrap();
        store.upsert_reviewer_profile(&built_in_default).unwrap();
        let engine = Engine::new(store.clone(), tmp.path().join("data"), &Config::default())
            .with_config_dir(Some(config_dir.clone()));

        engine
            .upsert_persona("custom", persona_request("Custom persona"))
            .await
            .unwrap();
        engine
            .upsert_persona("code", persona_request("Customized Code"))
            .await
            .unwrap();
        engine
            .upsert_persona("correctness", persona_request("Customized Correctness"))
            .await
            .unwrap();
        assert_eq!(
            store
                .list_built_in_reviewer_defaults()
                .unwrap()
                .into_iter()
                .find(|reviewer| reviewer.id == "correctness")
                .unwrap(),
            built_in_default
        );
        let workspace_persona = AgentPersona {
            id: "workspace-only".into(),
            display_name: "Workspace only".into(),
            group: trouve_protocol::PersonaGroup::General,
            system_prompt: "Inspect the workspace.".into(),
            allowed_tools: vec!["read_file".into()],
            read_only: true,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        };
        personas::upsert_user_persona(&workspace.join(".agents"), &workspace_persona).unwrap();
        assert!(
            personas::resolve_persona_infos(None, Some(&workspace))
                .iter()
                .any(|info| info.persona.id == "workspace-only" && info.origin == "workspace")
        );

        engine
            .store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/widgets".into(),
                mode: trouve_protocol::CodeReviewMode::Manual,
                model: Some("openai/reviewer".into()),
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                analyst_model: None,
                analyst_thinking_level: None,
                prompt: "preserve this".into(),
                reviewer_ids: Some(vec!["custom".into()]),
                routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Manual),
                semantic_routing: Some(false),
                included_reviewer_ids: Some(vec![
                    "custom".into(),
                    "code".into(),
                    "correctness".into(),
                    "workspace-only".into(),
                ]),
                excluded_reviewer_ids: Some(vec!["custom".into()]),
                reviewer_overrides: Some(vec![trouve_protocol::ReviewerOverride {
                    reviewer_id: "custom".into(),
                    model: None,
                    thinking_level: None,
                    prompt_mode: trouve_protocol::ReviewerPromptMode::Append,
                    prompt: "custom prompt".into(),
                }]),
            })
            .unwrap();

        engine.delete_persona("custom").await.unwrap();
        let repository = engine
            .store
            .list_code_review_repositories()
            .unwrap()
            .remove(0);
        assert_eq!(
            repository.reviewer_ids,
            crate::reviewers::default_reviewer_ids()
        );
        assert_eq!(
            repository.included_reviewer_ids,
            ["code", "correctness", "workspace-only"]
        );
        assert!(repository.excluded_reviewer_ids.is_empty());
        assert!(repository.reviewer_overrides.is_empty());
        assert_eq!(repository.prompt, "preserve this");
        assert_eq!(repository.model.as_deref(), Some("openai/reviewer"));

        let legacy_reviewer = trouve_protocol::ReviewerProfile {
            id: "legacy-reviewer".into(),
            name: "Legacy reviewer".into(),
            prompt: "Inspect legacy behavior.".into(),
            model: None,
            default_thinking_level: None,
            built_in: false,
        };
        engine
            .store
            .upsert_reviewer_profile(&legacy_reviewer)
            .unwrap();
        assert!(
            engine
                .code_review_reviewer_catalog()
                .unwrap()
                .iter()
                .any(|reviewer| reviewer.id == legacy_reviewer.id && !reviewer.built_in)
        );
        engine.delete_persona(&legacy_reviewer.id).await.unwrap();
        assert!(
            engine
                .store
                .list_custom_reviewer_profiles()
                .unwrap()
                .iter()
                .all(|reviewer| reviewer.id != legacy_reviewer.id)
        );

        engine.delete_persona("code").await.unwrap();
        engine.delete_persona("correctness").await.unwrap();
        let infos = engine.list_persona_infos(None).unwrap();
        let mut expected_builtin_ids = personas::builtin_personas()
            .into_iter()
            .map(|persona| persona.id)
            .chain(
                crate::reviewers::built_in_reviewers()
                    .into_iter()
                    .map(|reviewer| reviewer.id),
            )
            .collect::<Vec<_>>();
        for reviewer in engine
            .code_review_reviewer_catalog()
            .unwrap()
            .into_iter()
            .filter(|reviewer| reviewer.built_in)
        {
            if !expected_builtin_ids.contains(&reviewer.id) {
                expected_builtin_ids.push(reviewer.id);
            }
        }
        assert_eq!(
            infos
                .iter()
                .take(expected_builtin_ids.len())
                .map(|info| info.persona.id.clone())
                .collect::<Vec<_>>(),
            expected_builtin_ids
        );
        for (id, customized_name) in [
            ("code", "Customized Code"),
            ("correctness", "Customized Correctness"),
        ] {
            let info = infos.iter().find(|info| info.persona.id == id).unwrap();
            assert_eq!(info.origin, "builtin");
            assert_ne!(info.persona.display_name, customized_name);
        }

        assert!(matches!(
            engine.delete_persona("workspace-only").await,
            Err(EngineError::BadRequest(_))
        ));
        let repository = engine
            .store
            .list_code_review_repositories()
            .unwrap()
            .remove(0);
        assert!(
            repository
                .included_reviewer_ids
                .contains(&"workspace-only".to_string())
        );
    }

    #[tokio::test]
    async fn failed_executor_persona_deletion_keeps_repository_references_retryable() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let persona = AgentPersona {
            id: "custom".into(),
            display_name: "Custom".into(),
            group: trouve_protocol::PersonaGroup::General,
            system_prompt: "Review carefully.".into(),
            allowed_tools: Vec::new(),
            read_only: true,
            default_permission_mode: None,
            default_model: None,
            default_thinking_level: None,
        };
        personas::upsert_user_persona(&config_dir, &persona).unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/widgets".into(),
                mode: trouve_protocol::CodeReviewMode::Manual,
                model: None,
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                analyst_model: None,
                analyst_thinking_level: None,
                prompt: String::new(),
                reviewer_ids: Some(vec!["custom".into()]),
                routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Manual),
                semantic_routing: Some(false),
                included_reviewer_ids: None,
                excluded_reviewer_ids: None,
                reviewer_overrides: None,
            })
            .unwrap();
        let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let engine = Engine::new(store.clone(), tmp.path().join("data"), &Config::default())
            .with_config_dir(Some(config_dir.clone()))
            .with_executor(Arc::new(RejectingPersonaDeletionExecutor {
                attempted: attempted.clone(),
            }));

        let error = engine.delete_persona("custom").await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected persona deletion failure")
        );
        assert!(attempted.load(std::sync::atomic::Ordering::SeqCst));
        assert!(store.persona_deletion_pending("custom").unwrap());
        assert_eq!(
            store
                .list_code_review_repositories()
                .unwrap()
                .remove(0)
                .reviewer_ids,
            ["custom"]
        );
        assert!(
            personas::user_persona_file(&config_dir, "custom")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn github_permission_failures_require_reauthentication() {
        for message in [
            "Resource not accessible by integration",
            "The token has insufficient OAuth scope",
            "Bad credentials",
        ] {
            assert!(github_authentication_message(message));
        }

        let structured = octocrab::GraphqlError {
            message: "The operation was denied".into(),
            locations: None,
            path: None,
            extensions: Some(serde_json::json!({ "type": "INSUFFICIENT_SCOPES" })),
        };
        assert!(graphql_error_requires_reauthentication(&structured));
    }

    #[test]
    fn github_error_context_cannot_trigger_reauthentication() {
        assert!(matches!(
            github_engine_error(
                anyhow!("connection timed out")
                    .context("bad credentials were reported by an unrelated earlier request")
            ),
            EngineError::Internal(_)
        ));
    }

    #[tokio::test]
    async fn session_pr_verification_worker_is_idempotent_and_does_not_retain_engine() {
        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let weak = Arc::downgrade(&engine);

        engine.start_session_pr_verification_worker();
        engine.start_session_pr_verification_worker();
        assert!(
            engine
                .session_pr_verification_worker_started
                .load(Ordering::Acquire)
        );
        drop(engine);

        tokio::time::timeout(Duration::from_secs(1), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("verification worker retained the engine");
    }

    fn upload(name: &str, mime: &str, data: String) -> trouve_protocol::AttachmentUpload {
        trouve_protocol::AttachmentUpload {
            name: name.into(),
            mime: mime.into(),
            data,
        }
    }

    #[test]
    fn attachment_upload_validation_rejects_hostile_envelopes_before_decode() {
        let valid = upload("screen.png", "image/png", "aGVsbG8=".into());
        validate_attachment_uploads(std::slice::from_ref(&valid)).unwrap();

        let cases = [
            upload("../escape.png", "image/png", "aGVsbG8=".into()),
            upload("bad\nname.png", "image/png", "aGVsbG8=".into()),
            upload("screen.png", "text/html; charset=utf-8", "aGVsbG8=".into()),
            upload("screen.png", "image/", "aGVsbG8=".into()),
            upload("screen.png", "image/png", "aGVsbG8".into()),
            upload("screen.png", "image/png", "Zh==".into()),
        ];
        for hostile in cases {
            assert!(
                matches!(
                    validate_attachment_uploads(&[hostile]),
                    Err(EngineError::BadRequest(_))
                ),
                "hostile attachment envelope was accepted"
            );
        }

        assert!(matches!(
            validate_attachment_uploads(&vec![valid; MAX_ATTACHMENTS_PER_PROMPT + 1]),
            Err(EngineError::BadRequest(_))
        ));
    }

    #[test]
    fn attachment_upload_validation_enforces_item_and_aggregate_limits() {
        let oversized_bytes = MAX_ATTACHMENT_BYTES + (3 - MAX_ATTACHMENT_BYTES % 3);
        let oversized = upload(
            "large.bin",
            "application/octet-stream",
            "A".repeat(oversized_bytes / 3 * 4),
        );
        assert!(matches!(
            validate_attachment_uploads(&[oversized]),
            Err(EngineError::BadRequest(_))
        ));

        let seven_mib_divisible_by_three = 7 * 1024 * 1024 - 1;
        let encoded = "A".repeat(seven_mib_divisible_by_three / 3 * 4);
        let aggregate = (0..3)
            .map(|index| {
                upload(
                    &format!("part-{index}.bin"),
                    "application/octet-stream",
                    encoded.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            validate_attachment_uploads(&aggregate),
            Err(EngineError::BadRequest(_))
        ));
    }

    #[test]
    fn continuously_ready_backend_stream_yields_to_queued_steer_at_budget() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender.try_send(7_u8).unwrap();
        let mut receiver = Some(receiver);
        let mut pending = None;
        let mut consecutive_events = 0;

        // The first simultaneous backend event keeps causal priority.
        assert!(!reserve_ready_steer_after_event_budget(
            &mut receiver,
            &mut pending,
            &mut consecutive_events,
        ));
        for _ in 0..MAX_BACKEND_EVENTS_BEFORE_STEER {
            consecutive_events += 1;
        }

        // At the bound, the select disables its otherwise-continuously-ready
        // event branch for one iteration and consumes this reserved steer.
        assert!(reserve_ready_steer_after_event_budget(
            &mut receiver,
            &mut pending,
            &mut consecutive_events,
        ));
        assert_eq!(pending, Some(7));
        assert_eq!(consecutive_events, 0);
    }

    #[tokio::test]
    async fn deferred_attachment_lane_wait_yields_until_the_holder_releases() {
        let lane = Arc::new(tokio::sync::RwLock::new(()));
        let holder = lane.clone().write_owned().await;
        let mut waiter = Box::pin(lane.write_owned());

        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiter)
                .await
                .is_err()
        );
        drop(holder);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("lane waiter did not resume after the holder released");
    }

    #[tokio::test]
    async fn rejected_deferred_steering_returns_a_defined_error() {
        let (response, received) = tokio::sync::oneshot::channel();
        let mut pending = Some(SteerTurnCommand {
            content: "wait".into(),
            attachments: Vec::new(),
            attachment_rows: Vec::new(),
            attachment_cleanup: PreparedAttachmentCleanup::new(
                Store::open_in_memory().unwrap(),
                Arc::new(LocalToolExecutor::default()),
                PathBuf::new(),
                Vec::new(),
                None,
            ),
            response,
        });

        reject_pending_steer(&mut pending, "turn cancelled");

        assert_eq!(received.await.unwrap().unwrap_err(), "turn cancelled");
        assert!(pending.is_none());
    }

    fn init_engine_test_repo(path: &Path) {
        let run = |args: &[&str]| {
            let mut command = std::process::Command::new("git");
            command.arg("-C").arg(path).args(args);
            let output = trouve_process::output(&mut command).unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        std::fs::create_dir_all(path).unwrap();
        run(&["init", "-b", "main"]);
        std::fs::write(path.join("README.md"), "test\n").unwrap();
        run(&["add", "README.md"]);
        run(&[
            "-c",
            "user.name=trouve test",
            "-c",
            "user.email=trouve@example.invalid",
            "commit",
            "-m",
            "initial",
        ]);
    }

    fn sqlite_busy_error() -> anyhow::Error {
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("checkpoint-contention.sqlite3");
        let holder = rusqlite::Connection::open(&database).unwrap();
        holder
            .execute_batch(
                "CREATE TABLE value (id INTEGER); BEGIN EXCLUSIVE; INSERT INTO value VALUES (1);",
            )
            .unwrap();
        let contender = rusqlite::Connection::open(&database).unwrap();
        contender.busy_timeout(Duration::ZERO).unwrap();
        let error = contender
            .query_row("SELECT COUNT(*) FROM value", [], |row| row.get::<_, i64>(0))
            .unwrap_err();
        holder.execute_batch("ROLLBACK").unwrap();
        anyhow::Error::new(error)
    }

    fn sqlite_locked_error() -> anyhow::Error {
        anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            Some("database table is locked".into()),
        ))
    }

    #[tokio::test]
    async fn checkpoint_sqlite_retry_recovers_from_transient_contention() {
        let mut first_error = Some(sqlite_locked_error());
        let mut calls = 0usize;
        let result = retry_checkpoint_sqlite(
            &tokio_util::sync::CancellationToken::new(),
            "test checkpoint operation",
            &[Duration::ZERO],
            || {
                calls += 1;
                match first_error.take() {
                    Some(error) => Err(error),
                    None => Ok(42),
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(result, Some(42));
        assert_eq!(calls, 2);
    }

    #[tokio::test]
    async fn checkpoint_sqlite_retry_is_bounded_and_cancellation_aware() {
        let mut errors = [sqlite_locked_error(), sqlite_locked_error()].into_iter();
        let error = retry_checkpoint_sqlite(
            &tokio_util::sync::CancellationToken::new(),
            "test checkpoint operation",
            &[Duration::ZERO],
            || Err::<(), _>(errors.next().unwrap()),
        )
        .await
        .unwrap_err();
        assert!(is_transient_sqlite_contention(&error));

        // SQLITE_BUSY has already exhausted the connection's busy timeout,
        // so it is skippable checkpoint contention but is not multiplied by
        // the SQLITE_LOCKED retry schedule.
        let mut busy = Some(sqlite_busy_error());
        let mut busy_calls = 0usize;
        let error = retry_checkpoint_sqlite(
            &tokio_util::sync::CancellationToken::new(),
            "busy checkpoint operation",
            &[Duration::ZERO],
            || {
                busy_calls += 1;
                Err::<(), _>(busy.take().unwrap())
            },
        )
        .await
        .unwrap_err();
        assert!(is_transient_sqlite_contention(&error));
        assert!(!is_immediate_sqlite_lock(&error));
        assert_eq!(busy_calls, 1);

        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let skipped = retry_checkpoint_sqlite(&cancel, "cancelled operation", &[], || {
            panic!("a cancelled checkpoint operation must not run")
        })
        .await
        .unwrap();
        assert_eq!(skipped, None::<()>);
    }

    #[tokio::test]
    async fn codex_bridge_router_correlates_explicit_vendor_and_call_identity() {
        let router = BridgedToolOwnerRouter::default();
        router.begin_root("root");
        router
            .bind_vendor_thread("root", "vendor-a", "owner-a")
            .unwrap();
        router
            .bind_vendor_thread("root", "vendor-b", "owner-b")
            .unwrap();

        assert!(matches!(
            router.register_vendor_owner("root", "vendor-a"),
            CodexVendorOwnerRegistration::Immediate(owner) if owner == "owner-a"
        ));

        // HTTP first: wait for app-server's item identity, independent of
        // payload equality or any sibling call's ordering.
        let CodexCallValidationRegistration::Pending { receiver, .. } =
            router.register_call_validation("root", "vendor-a", "owner-a", "call-a")
        else {
            panic!("request-first identity should wait for its wrapper");
        };
        assert!(router.announce_wrapper("root", "vendor-a", "owner-a", "call-a"));
        assert_eq!(receiver.await.unwrap(), CodexCallValidationOutcome::Matched);

        // Wrapper first: the same rendezvous works in the opposite order.
        assert!(router.announce_wrapper("root", "vendor-b", "owner-b", "call-b"));
        assert!(matches!(
            router.register_call_validation("root", "vendor-b", "owner-b", "call-b"),
            CodexCallValidationRegistration::Immediate
        ));

        // A call id can authorize only its announced vendor owner and only
        // once, even when sibling calls carry byte-identical tool payloads.
        assert!(router.announce_wrapper("root", "vendor-a", "owner-a", "call-owner-a"));
        assert!(matches!(
            router.register_call_validation("root", "vendor-b", "owner-b", "call-owner-a"),
            CodexCallValidationRegistration::MismatchedOwner
        ));
        assert!(matches!(
            router.register_call_validation("root", "vendor-b", "owner-b", "call-b"),
            CodexCallValidationRegistration::Replayed
        ));

        // A not-yet-announced collaborator may rendezvous briefly, but an
        // external id is not borrowed from another root.
        let CodexVendorOwnerRegistration::Pending {
            id,
            receiver: unknown,
        } = router.register_vendor_owner("root", "external-vendor")
        else {
            panic!("unknown vendor ids should never resolve immediately");
        };
        router.abandon_vendor_owner("root", "external-vendor", id);
        assert!(unknown.await.is_err());

        router.clear_root("root");
        assert!(matches!(
            router.register_vendor_owner("root", "vendor-a"),
            CodexVendorOwnerRegistration::InactiveRoot
        ));
        assert!(matches!(
            router.register_call_validation("root", "vendor-a", "owner-a", "stale-call"),
            CodexCallValidationRegistration::InactiveRoot
        ));
    }

    struct DiscoveryProbeExecutor {
        full_catalog_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    fn probe_spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: name.into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for DiscoveryProbeExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            self.full_catalog_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            vec![probe_spec("mcp__trusted__external")]
        }

        async fn native_specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            vec![probe_spec("search"), probe_spec("find_related")]
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            Some(false)
        }

        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::error("not used")
        }
    }

    #[tokio::test]
    async fn minimal_bridge_listing_never_discovers_external_mcp_tools() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_minimal_bridge".into(),
            name: "minimal bridge".into(),
            path: data.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_minimal_bridge".into(),
            workspace_id: workspace.id,
            title: "Minimal bridge".into(),
            branch: "trouve/minimal-bridge".into(),
            worktree_path: data.path().to_string_lossy().into_owned(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_minimal_bridge".into(),
            session_id: session.id,
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "codex/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Yolo,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let full_catalog_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let engine = Engine::new(store, data.path().into(), &Config::default()).with_executor(
            Arc::new(DiscoveryProbeExecutor {
                full_catalog_calls: full_catalog_calls.clone(),
            }),
        );
        let _cancel = engine.register_cancel(&thread.id);

        let minimal = engine.bridged_tool_specs(&thread.id, false).await.unwrap();
        assert_eq!(
            full_catalog_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "minimal listing must not enter external MCP discovery"
        );
        assert_eq!(
            minimal
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["search", "find_related", "ask_question"])
        );

        let full = engine.bridged_tool_specs(&thread.id, true).await.unwrap();
        assert_eq!(
            full_catalog_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            full.iter()
                .any(|spec| spec.name == "mcp__trusted__external")
        );
        engine.clear_cancel(&thread.id);
    }

    #[test]
    fn automated_review_tool_budget_is_atomic_across_parallel_reservations() {
        let budgets = Arc::new(AutomatedReviewToolBudgets::default());
        let guard = budgets.arm("review-thread", 4).unwrap();
        let allowed = std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|_| {
                    let budgets = budgets.clone();
                    scope.spawn(move || budgets.reserve("review-thread").is_ok())
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .filter(|allowed| *allowed)
                .count()
        });
        assert_eq!(allowed, 4);
        assert!(budgets.reserve("review-thread").is_err());

        drop(guard);
        assert!(budgets.reserve("review-thread").is_ok());

        let pre_dispatch = budgets.arm("zero-call-review", 0).unwrap();
        let dispatcher = budgets
            .claim_dispatch("zero-call-review")
            .expect("dispatcher claims the armed budget");
        drop(pre_dispatch);
        assert!(budgets.reserve("zero-call-review").is_err());
        drop(dispatcher);
        assert!(budgets.reserve("zero-call-review").is_ok());
    }

    #[test]
    fn automated_review_vendor_tools_fail_closed_without_a_full_bridge() {
        assert!(
            enforce_automated_review_backend_boundary(false, true, false, false, "unsafe").is_ok()
        );
        assert!(
            enforce_automated_review_backend_boundary(true, false, false, false, "unsafe").is_ok()
        );
        assert!(
            enforce_automated_review_backend_boundary(true, true, true, false, "codex").is_ok()
        );
        assert!(
            enforce_automated_review_backend_boundary(true, true, false, true, "cursor").is_ok()
        );
        assert!(
            enforce_automated_review_backend_boundary(true, true, false, false, "unsafe").is_err()
        );

        assert!(vendor_tool_uses_automated_review_budget(
            true,
            "read_file",
            true
        ));
        assert!(!vendor_tool_uses_automated_review_budget(
            true,
            "read_file",
            false
        ));
        assert!(!vendor_tool_uses_automated_review_budget(
            false, "search", true
        ));
        assert!(!vendor_tool_uses_automated_review_budget(
            true,
            "mcp__trouve__read_file",
            true
        ));
    }

    #[test]
    fn backend_collaborator_claims_never_overwrite_an_active_dispatcher() {
        let active_threads = Mutex::new(HashMap::from([(
            "already-running".to_string(),
            "session-a".to_string(),
        )]));
        {
            let mut claims = BackendCollaboratorClaims::new(&active_threads);
            assert!(!claims.claim("already-running", "session-b"));
            assert!(claims.claim("new-collaborator", "session-b"));
            let active = active_threads.lock().unwrap();
            assert_eq!(
                active.get("already-running").map(String::as_str),
                Some("session-a")
            );
            assert_eq!(
                active.get("new-collaborator").map(String::as_str),
                Some("session-b")
            );
        }
        let active = active_threads.lock().unwrap();
        assert_eq!(
            active.get("already-running").map(String::as_str),
            Some("session-a")
        );
        assert!(!active.contains_key("new-collaborator"));
    }

    #[test]
    fn only_first_party_codex_mcp_items_are_bridge_wrappers() {
        let args = serde_json::json!({
            "type": "mcpToolCall",
            "server": "trouve",
            "tool": "read_file",
            "arguments": { "path": "README.md" }
        });
        let (tool, nested) = trouve_bridge_wrapper_call("mcpToolCall", &args).unwrap();
        assert_eq!(tool, "read_file");
        assert_eq!(nested, &serde_json::json!({ "path": "README.md" }));
        assert!(
            trouve_bridge_wrapper_call(
                "mcpToolCall",
                &serde_json::json!({
                    "server": "github",
                    "tool": "get_issue",
                    "arguments": {}
                })
            )
            .is_none()
        );
        assert!(trouve_bridge_wrapper_call("commandExecution", &args).is_none());
        assert!(trouve_direct_bridge_call("mcp__trouve__read_file"));
        assert!(!trouve_direct_bridge_call("mcp__github__get_issue"));
        assert!(!trouve_direct_bridge_call("read_file"));
    }

    struct CatalogTestProvider {
        live_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    fn catalog_test_model(id: &str, display_name: &str) -> trouve_protocol::ModelInfo {
        trouve_protocol::ModelInfo {
            id: id.into(),
            display_name: display_name.into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({}),
        }
    }

    #[async_trait::async_trait]
    impl Provider for CatalogTestProvider {
        fn id(&self) -> &str {
            "catalog-test"
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_test_model(
                "catalog-test/static",
                "Static catalog model",
            )]
        }

        async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
            self.live_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            vec![catalog_test_model(
                "catalog-test/live",
                "Live discovered model",
            )]
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[trouve_providers::Message],
            _tools: &[trouve_providers::ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<trouve_providers::EventStream, trouve_providers::ProviderError> {
            unreachable!("model catalog tests never start a provider turn")
        }
    }

    struct BlockingToolExecutor {
        started: tokio::sync::mpsc::UnboundedSender<String>,
        releases: Arc<tokio::sync::Semaphore>,
    }

    struct McpInvalidationProbeExecutor {
        started: Arc<tokio::sync::Semaphore>,
        releases: Arc<tokio::sync::Semaphore>,
        names: Arc<Mutex<Vec<String>>>,
        fail_after_commit: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl ToolExecutor for McpInvalidationProbeExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            None
        }

        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::error("not used")
        }

        async fn mutate_mcp_config(
            &self,
            request: &McpConfigMutationRequest,
        ) -> Result<McpConfigMutationOutcome, String> {
            let outcome = match &request.mutation {
                McpConfigMutation::Upsert(config) => {
                    crate::mcp::upsert_server(&request.path, &request.name, config)
                        .map_err(|error| format!("{error:#}"))?;
                    McpConfigMutationOutcome::Applied
                }
                McpConfigMutation::SetEnabled(enabled) => {
                    if crate::mcp::set_server_enabled(&request.path, &request.name, *enabled)
                        .map_err(|error| format!("{error:#}"))?
                    {
                        McpConfigMutationOutcome::Applied
                    } else {
                        McpConfigMutationOutcome::NotFound
                    }
                }
                McpConfigMutation::Remove => {
                    crate::mcp::remove_server(&request.path, &request.name)
                        .map_err(|error| format!("{error:#}"))?;
                    McpConfigMutationOutcome::Applied
                }
            };
            if outcome == McpConfigMutationOutcome::NotFound {
                return Ok(outcome);
            }
            self.names.lock().unwrap().push(request.name.clone());
            self.started.add_permits(1);
            self.releases
                .clone()
                .acquire_owned()
                .await
                .unwrap()
                .forget();
            if self.fail_after_commit.swap(false, Ordering::SeqCst) {
                tracing::warn!(
                    server = %request.name,
                    "injected post-commit MCP cleanup failure left the server quarantined"
                );
            }
            Ok(outcome)
        }
    }

    async fn await_mcp_invalidation_start(
        started: &Arc<tokio::sync::Semaphore>,
        task: &mut tokio::task::JoinHandle<Result<(), EngineError>>,
    ) {
        tokio::select! {
            permit = started.clone().acquire_owned() => {
                permit.expect("MCP invalidation probe semaphore closed").forget();
            }
            result = task => {
                panic!("MCP settings mutation exited before cache invalidation started: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_secs(5)) => {
                panic!("MCP settings mutation did not reach cache invalidation within five seconds");
            }
        }
    }

    struct SuccessfulTodoExecutor;

    #[async_trait::async_trait]
    impl ToolExecutor for SuccessfulTodoExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            Some(false)
        }

        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::ok(serde_json::json!({"todos": [
                {"id": "external", "content": "External", "status": "completed"}
            ]}))
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for BlockingToolExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn tool_mutates(&self, name: &str) -> Option<bool> {
            Some(name.starts_with("write_"))
        }

        async fn execute(
            &self,
            _ctx: &ToolCtx,
            name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            self.started.send(name.to_string()).unwrap();
            self.releases.acquire().await.unwrap().forget();
            ToolResult::ok(serde_json::json!({ "tool": name }))
        }
    }

    struct CancellationAwareToolExecutor {
        started: Arc<tokio::sync::Semaphore>,
        cleanup_started: Arc<tokio::sync::Semaphore>,
        cleanup_release: Arc<tokio::sync::Semaphore>,
    }

    struct FailingSessionCreationExecutor(LocalToolExecutor);

    #[async_trait::async_trait]
    impl ToolExecutor for FailingSessionCreationExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            None
        }

        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::error("not used")
        }

        async fn create_session_worktree(
            &self,
            request: &crate::tools::SessionWorktreeCreate,
        ) -> Result<crate::tools::SessionWorktreeCreation, String> {
            let creation =
                <LocalToolExecutor as ToolExecutor>::create_session_worktree(&self.0, request)
                    .await?;
            drop(creation);
            Err("injected checkpoint failure".into())
        }
    }

    #[derive(Clone, Copy)]
    enum SessionFinalizeBehavior {
        Complete,
        Block,
        Fail,
    }

    struct SessionCreationProbeExecutor {
        block_create: bool,
        finalize_behavior: SessionFinalizeBehavior,
        create_started: Arc<tokio::sync::Semaphore>,
        create_release: Arc<tokio::sync::Semaphore>,
        finalize_started: Arc<tokio::sync::Semaphore>,
        finalize_release: Arc<tokio::sync::Semaphore>,
        rollback_count: Arc<std::sync::atomic::AtomicUsize>,
        finalize_count: Arc<std::sync::atomic::AtomicUsize>,
        owned_artifact: Arc<std::sync::atomic::AtomicBool>,
        requested_worktrees: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl SessionCreationProbeExecutor {
        fn new(block_create: bool, finalize_behavior: SessionFinalizeBehavior) -> Self {
            Self {
                block_create,
                finalize_behavior,
                create_started: Arc::new(tokio::sync::Semaphore::new(0)),
                create_release: Arc::new(tokio::sync::Semaphore::new(0)),
                finalize_started: Arc::new(tokio::sync::Semaphore::new(0)),
                finalize_release: Arc::new(tokio::sync::Semaphore::new(0)),
                rollback_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                finalize_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                owned_artifact: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                requested_worktrees: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for SessionCreationProbeExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            None
        }
        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::error("not used")
        }
        async fn create_session_worktree(
            &self,
            request: &crate::tools::SessionWorktreeCreate,
        ) -> Result<crate::tools::SessionWorktreeCreation, String> {
            self.requested_worktrees
                .lock()
                .unwrap()
                .push(request.worktree.clone());
            self.owned_artifact
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let rollback_count = self.rollback_count.clone();
            let owned_artifact = self.owned_artifact.clone();
            let finalize_count = self.finalize_count.clone();
            let creation = crate::tools::SessionWorktreeCreation::guarded(
                request
                    .base_ref
                    .clone()
                    .unwrap_or_else(|| "probe-base".into()),
                "0123456789abcdef0123456789abcdef01234567".into(),
                move || {
                    rollback_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    owned_artifact.store(false, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
                move || {
                    finalize_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                },
            );
            if self.block_create {
                self.create_started.add_permits(1);
                self.create_release
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|e| e.to_string())?
                    .forget();
            }
            Ok(creation)
        }
        async fn finalize_session_worktree(
            &self,
            creation: crate::tools::SessionWorktreeCreation,
        ) -> Result<(), String> {
            match self.finalize_behavior {
                SessionFinalizeBehavior::Complete => creation.finalize(),
                SessionFinalizeBehavior::Fail => Err("injected finalization failure".into()),
                SessionFinalizeBehavior::Block => {
                    self.finalize_started.add_permits(1);
                    self.finalize_release
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|e| e.to_string())?
                        .forget();
                    creation.finalize()
                }
            }
        }
        async fn rollback_session_worktree(
            &self,
            request: crate::tools::SessionWorktreeRollback,
        ) -> Result<(), String> {
            request.creation.rollback()
        }
    }

    #[async_trait::async_trait]
    impl ToolExecutor for CancellationAwareToolExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }

        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            Some(true)
        }

        async fn execute(
            &self,
            ctx: &ToolCtx,
            _name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            self.started.add_permits(1);
            ctx.cancel.cancelled().await;
            self.cleanup_started.add_permits(1);
            self.cleanup_release
                .clone()
                .acquire_owned()
                .await
                .unwrap()
                .forget();
            ToolResult::error("tool cancelled after cleanup")
        }
    }

    #[tokio::test]
    async fn mcp_settings_persist_before_waiting_for_cache_invalidation() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let releases = Arc::new(tokio::sync::Semaphore::new(0));
        let names = Arc::new(Mutex::new(Vec::new()));
        let fail_after_commit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let config = Config {
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Arc::new(
            Engine::new(
                Store::open_in_memory().unwrap(),
                tmp.path().join("data"),
                &config,
            )
            .with_config_dir(Some(config_dir.clone()))
            .with_executor(Arc::new(McpInvalidationProbeExecutor {
                started: started.clone(),
                releases: releases.clone(),
                names: names.clone(),
                fail_after_commit: fail_after_commit.clone(),
            })),
        );
        let config_path = crate::mcp::user_config_path(&config_dir);

        let request = trouve_protocol::UpsertMcpServerRequest {
            scope: "user".into(),
            workspace_id: None,
            command: "first".into(),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            enabled: Some(true),
        };
        assert!(engine.upsert_mcp_server("", &request).await.is_err());
        assert!(started.try_acquire().is_err());

        let task_engine = engine.clone();
        let mut task =
            tokio::spawn(async move { task_engine.upsert_mcp_server("docs", &request).await });
        await_mcp_invalidation_start(&started, &mut task).await;
        let servers = crate::mcp::read_servers(&config_path);
        assert_eq!(servers["docs"].command, "first");
        assert!(!servers["docs"].disabled);
        assert!(!task.is_finished());
        releases.add_permits(1);
        task.await.unwrap().unwrap();

        fail_after_commit.store(true, Ordering::SeqCst);
        let task_engine = engine.clone();
        let mut committed = tokio::spawn(async move {
            task_engine
                .upsert_mcp_server(
                    "committed",
                    &trouve_protocol::UpsertMcpServerRequest {
                        scope: "user".into(),
                        workspace_id: None,
                        command: "committed-mcp".into(),
                        args: Vec::new(),
                        env: std::collections::BTreeMap::new(),
                        enabled: Some(true),
                    },
                )
                .await
        });
        await_mcp_invalidation_start(&started, &mut committed).await;
        assert_eq!(
            crate::mcp::read_servers(&config_path)["committed"].command,
            "committed-mcp"
        );
        releases.add_permits(1);
        committed.await.unwrap().unwrap();
        assert_eq!(
            crate::mcp::read_servers(&config_path)["committed"].command,
            "committed-mcp",
            "an invalidation failure must not retry or roll back the committed RMW"
        );

        let task_engine = engine.clone();
        let mut task = tokio::spawn(async move {
            task_engine
                .set_mcp_server_enabled(
                    "docs",
                    &trouve_protocol::SetMcpServerEnabledRequest {
                        scope: "user".into(),
                        workspace_id: None,
                        enabled: false,
                    },
                )
                .await
        });
        await_mcp_invalidation_start(&started, &mut task).await;
        assert!(crate::mcp::read_servers(&config_path)["docs"].disabled);
        assert!(!task.is_finished());
        releases.add_permits(1);
        task.await.unwrap().unwrap();

        let task_engine = engine.clone();
        let mut task = tokio::spawn(async move {
            task_engine
                .set_mcp_server_enabled(
                    "docs",
                    &trouve_protocol::SetMcpServerEnabledRequest {
                        scope: "user".into(),
                        workspace_id: None,
                        enabled: true,
                    },
                )
                .await
        });
        await_mcp_invalidation_start(&started, &mut task).await;
        assert!(!crate::mcp::read_servers(&config_path)["docs"].disabled);
        assert!(!task.is_finished());
        releases.add_permits(1);
        task.await.unwrap().unwrap();

        let task_engine = engine.clone();
        let mut task = tokio::spawn(async move {
            task_engine
                .upsert_mcp_server(
                    "docs",
                    &trouve_protocol::UpsertMcpServerRequest {
                        scope: "user".into(),
                        workspace_id: None,
                        command: "replacement".into(),
                        args: vec!["--new".into()],
                        env: std::collections::BTreeMap::new(),
                        enabled: Some(true),
                    },
                )
                .await
        });
        await_mcp_invalidation_start(&started, &mut task).await;
        let servers = crate::mcp::read_servers(&config_path);
        assert_eq!(servers["docs"].command, "replacement");
        assert_eq!(servers["docs"].args, ["--new"]);
        assert!(!task.is_finished());
        releases.add_permits(1);
        task.await.unwrap().unwrap();

        let task_engine = engine.clone();
        let mut task =
            tokio::spawn(async move { task_engine.delete_mcp_server("docs", "user", None).await });
        await_mcp_invalidation_start(&started, &mut task).await;
        assert!(!crate::mcp::read_servers(&config_path).contains_key("docs"));
        assert!(!task.is_finished());
        releases.add_permits(1);
        task.await.unwrap().unwrap();

        let missing = engine
            .set_mcp_server_enabled(
                "missing",
                &trouve_protocol::SetMcpServerEnabledRequest {
                    scope: "user".into(),
                    workspace_id: None,
                    enabled: true,
                },
            )
            .await;
        assert!(matches!(missing, Err(EngineError::NotFound(_))));
        assert!(started.try_acquire().is_err());
        assert_eq!(
            names.lock().unwrap().as_slice(),
            ["docs", "committed", "docs", "docs", "docs", "docs"]
        );
    }

    struct RejectingMcpConfigExecutor;

    #[async_trait::async_trait]
    impl ToolExecutor for RejectingMcpConfigExecutor {
        async fn specs(&self, _ctx: &ToolCtx) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn tool_mutates(&self, _name: &str) -> Option<bool> {
            None
        }
        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _name: &str,
            _args: &serde_json::Value,
        ) -> ToolResult {
            ToolResult::error("not used")
        }
    }

    #[tokio::test]
    async fn custom_executor_cannot_fall_through_to_host_mcp_config_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        let config_path = crate::mcp::user_config_path(&config_dir);
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            tmp.path().join("data"),
            &Config::default(),
        )
        .with_config_dir(Some(config_dir))
        .with_executor(Arc::new(RejectingMcpConfigExecutor));

        let error = engine
            .upsert_mcp_server(
                "docs",
                &trouve_protocol::UpsertMcpServerRequest {
                    scope: "user".into(),
                    workspace_id: None,
                    command: "docs-mcp".into(),
                    args: Vec::new(),
                    env: std::collections::BTreeMap::new(),
                    enabled: Some(true),
                },
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("unavailable"), "{error}");
        assert!(!config_path.exists(), "engine bypassed the custom executor");
    }

    #[test]
    fn session_branches_default_to_short_ids_and_can_include_title_slugs() {
        assert_eq!(
            session_branch_name("Fix the Login Bug", "se_abc123def456", false),
            "trouve/abc123"
        );
        assert_eq!(
            session_branch_name("Fix the Login Bug", "se_abc123def456", true),
            "trouve/fix-the-login-bug-abc123"
        );
    }

    #[tokio::test]
    async fn failed_session_creation_rolls_back_worktree_branch_and_database_rows() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let data_dir = temp.path().join("data");
        init_engine_test_repo(&repo);
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_session_rollback".into(),
            name: "rollback".into(),
            path: repo.to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let engine = Engine::new(store.clone(), data_dir.clone(), &Config::default())
            .with_executor(Arc::new(FailingSessionCreationExecutor(
                LocalToolExecutor::default(),
            )));

        let error = engine
            .create_session(CreateSessionRequest {
                workspace_id: workspace.id.clone(),
                idempotency_key: None,
                title: Some("Must roll back".into()),
                base_ref: Some("main".into()),
                checkout_ref: None,
                fetch_latest: false,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("injected checkpoint failure"));
        assert!(store.list_sessions(Some(&workspace.id)).unwrap().is_empty());
        assert_eq!(git::list_branches(&repo).unwrap(), ["main"]);
        let worktree_root = data_dir.join("worktrees");
        assert!(
            !worktree_root.exists() || std::fs::read_dir(worktree_root).unwrap().next().is_none(),
            "failed creation left a worktree directory"
        );
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(&repo).args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/trouve/checkpoints/",
        ]);
        let refs = trouve_process::output(&mut command).unwrap();
        assert!(refs.status.success());
        assert!(
            refs.stdout.is_empty(),
            "failed creation left a checkpoint ref"
        );
    }

    fn session_probe_engine(
        probe: Arc<SessionCreationProbeExecutor>,
    ) -> (tempfile::TempDir, Store, Workspace, Arc<Engine>) {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host-workspace");
        std::fs::create_dir(&host).unwrap();
        std::fs::write(host.join("sentinel"), "unchanged").unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_session_probe".into(),
            name: "probe".into(),
            path: host.to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let engine = Arc::new(
            Engine::new(store.clone(), temp.path().join("data"), &Config::default())
                .with_executor(probe),
        );
        (temp, store, workspace, engine)
    }

    fn session_probe_request(workspace: &Workspace) -> CreateSessionRequest {
        CreateSessionRequest {
            workspace_id: workspace.id.clone(),
            idempotency_key: None,
            title: Some("Executor boundary".into()),
            base_ref: Some("main".into()),
            checkout_ref: None,
            fetch_latest: false,
        }
    }

    #[tokio::test]
    async fn custom_session_executor_never_mutates_the_host_workspace() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            false,
            SessionFinalizeBehavior::Complete,
        ));
        let (temp, store, workspace, engine) = session_probe_engine(probe.clone());

        let session = engine
            .create_session(session_probe_request(&workspace))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(temp.path().join("host-workspace/sentinel")).unwrap(),
            "unchanged"
        );
        assert!(!Path::new(&session.worktree_path).exists());
        assert!(store.session(&session.id).unwrap().is_some());
        assert_eq!(
            probe
                .rollback_count
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            probe
                .finalize_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(probe.requested_worktrees.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn session_creation_idempotency_key_returns_the_committed_session() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            false,
            SessionFinalizeBehavior::Complete,
        ));
        let (_temp, store, workspace, engine) = session_probe_engine(probe.clone());
        let mut request = session_probe_request(&workspace);
        request.idempotency_key = Some("create-session-once".into());

        let first = engine.create_session(request.clone()).await.unwrap();
        let retry = engine.create_session(request).await.unwrap();

        assert_eq!(retry.id, first.id);
        assert_eq!(store.list_sessions(Some(&workspace.id)).unwrap().len(), 1);
        assert_eq!(probe.requested_worktrees.lock().unwrap().len(), 1);
        assert_eq!(
            store
                .session_by_create_idempotency_key("create-session-once")
                .unwrap()
                .map(|(session, _)| session.id),
            Some(first.id),
        );
    }

    #[tokio::test]
    async fn session_creation_idempotency_key_rejects_a_different_request() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            false,
            SessionFinalizeBehavior::Complete,
        ));
        let (_temp, store, workspace, engine) = session_probe_engine(probe.clone());
        let mut request = session_probe_request(&workspace);
        request.idempotency_key = Some("create-session-fingerprint".into());
        engine.create_session(request.clone()).await.unwrap();
        request.title = Some("Different session".into());

        let error = engine.create_session(request).await.unwrap_err();

        assert!(matches!(error, EngineError::Conflict(_)));
        assert_eq!(store.list_sessions(Some(&workspace.id)).unwrap().len(), 1);
        assert_eq!(probe.requested_worktrees.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn concurrent_session_creation_retries_share_one_worktree_attempt() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            true,
            SessionFinalizeBehavior::Complete,
        ));
        let (_temp, store, workspace, engine) = session_probe_engine(probe.clone());
        let mut request = session_probe_request(&workspace);
        request.idempotency_key = Some("create-session-concurrently".into());
        let first = tokio::spawn({
            let engine = engine.clone();
            let request = request.clone();
            async move { engine.create_session(request).await.unwrap() }
        });
        probe
            .create_started
            .clone()
            .acquire_owned()
            .await
            .unwrap()
            .forget();
        let retry = tokio::spawn({
            let engine = engine.clone();
            async move { engine.create_session(request).await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        probe.create_release.add_permits(2);

        let (first, retry) = tokio::join!(first, retry);
        assert_eq!(retry.unwrap().id, first.unwrap().id);
        assert_eq!(store.list_sessions(Some(&workspace.id)).unwrap().len(), 1);
        assert_eq!(probe.requested_worktrees.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancelled_session_creation_retry_waits_for_attempt_cleanup() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            true,
            SessionFinalizeBehavior::Complete,
        ));
        let (_temp, store, workspace, engine) = session_probe_engine(probe.clone());
        let mut request = session_probe_request(&workspace);
        request.idempotency_key = Some("create-session-after-cancellation".into());
        let first = tokio::spawn({
            let engine = engine.clone();
            let request = request.clone();
            async move { engine.create_session(request).await }
        });
        probe
            .create_started
            .clone()
            .acquire_owned()
            .await
            .unwrap()
            .forget();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        let retry = tokio::spawn({
            let engine = engine.clone();
            async move { engine.create_session(request).await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(probe.requested_worktrees.lock().unwrap().len(), 1);

        probe.create_release.add_permits(1);
        probe
            .create_started
            .clone()
            .acquire_owned()
            .await
            .unwrap()
            .forget();
        assert_eq!(
            probe
                .rollback_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        probe.create_release.add_permits(1);

        let session = retry.await.unwrap();
        assert_eq!(store.list_sessions(Some(&workspace.id)).unwrap().len(), 1);
        assert_eq!(store.session(&session.id).unwrap().unwrap().id, session.id);
        assert_eq!(probe.requested_worktrees.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn aborting_session_creation_before_receipt_delivery_cleans_late_receipt() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            true,
            SessionFinalizeBehavior::Complete,
        ));
        let (_temp, store, workspace, engine) = session_probe_engine(probe.clone());
        let task = tokio::spawn({
            let engine = engine.clone();
            let request = session_probe_request(&workspace);
            async move { engine.create_session(request).await }
        });
        probe
            .create_started
            .clone()
            .acquire_owned()
            .await
            .unwrap()
            .forget();
        assert!(
            probe
                .owned_artifact
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        probe.create_release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while probe
                .owned_artifact
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("late session creation receipt was not cleaned");
        assert_eq!(
            probe
                .rollback_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(store.list_sessions(Some(&workspace.id)).unwrap().is_empty());
    }

    #[tokio::test]
    async fn aborting_session_finalization_preserves_the_durable_session() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            false,
            SessionFinalizeBehavior::Block,
        ));
        let (_temp, store, workspace, engine) = session_probe_engine(probe.clone());
        let task = tokio::spawn({
            let engine = engine.clone();
            let request = session_probe_request(&workspace);
            async move { engine.create_session(request).await }
        });
        probe
            .finalize_started
            .clone()
            .acquire_owned()
            .await
            .unwrap()
            .forget();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            probe
                .rollback_count
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            probe
                .finalize_count
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(store.list_sessions(Some(&workspace.id)).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn session_finalization_failure_cannot_roll_back_durable_state() {
        let probe = Arc::new(SessionCreationProbeExecutor::new(
            false,
            SessionFinalizeBehavior::Fail,
        ));
        let (_temp, store, workspace, engine) = session_probe_engine(probe.clone());
        let session = engine
            .create_session(session_probe_request(&workspace))
            .await
            .unwrap();

        assert!(store.session(&session.id).unwrap().is_some());
        assert_eq!(
            probe
                .rollback_count
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn spawn_output_wait_is_immediately_cancellation_aware() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_spawn_cancel".into(),
            name: "spawn cancel".into(),
            path: temp.path().to_string_lossy().into_owned(),
        };
        let session = Session {
            id: "se_spawn_cancel".into(),
            workspace_id: workspace.id.clone(),
            title: "spawn cancel".into(),
            branch: "main".into(),
            worktree_path: workspace.path.clone(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        let parent = Thread {
            id: "th_spawn_cancel_parent".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "provider/model".into(),
            model_options: serde_json::Map::new(),
            permission_mode: trouve_protocol::PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        let child = Thread {
            id: "th_spawn_cancel_child".into(),
            parent_thread_id: Some(parent.id.clone()),
            spawned: true,
            ..parent.clone()
        };
        store.insert_workspace(&workspace).unwrap();
        store.insert_session(&session).unwrap();
        store
            .insert_thread(&parent, &serde_json::Map::new())
            .unwrap();
        store
            .insert_spawned_thread(&child, &serde_json::Map::new(), &parent.id, "thread")
            .unwrap();
        let engine = Arc::new(Engine::new(
            store,
            temp.path().join("data"),
            &Config::default(),
        ));
        engine
            .active_threads
            .lock()
            .unwrap()
            .insert(child.id.clone(), session.id.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            trigger.cancel();
        });

        let result = tokio::time::timeout(
            Duration::from_millis(200),
            engine.handle_spawn_tool(
                &session,
                &parent,
                &personas::fallback_persona(),
                "spawn_output",
                &serde_json::json!({"thread_id": child.id, "wait_ms": 180_000}),
                &cancel,
            ),
        )
        .await
        .expect("spawn_output did not wake promptly on cancellation")
        .unwrap_err();
        assert!(result.to_string().contains("cancelled"));
    }

    #[test]
    fn queued_attachment_removal_cleans_index_rows_and_owned_files() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let attachment_dir = data_dir.join("attachments");
        std::fs::create_dir_all(&attachment_dir).unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_attachment_cleanup".into(),
            name: "attachments".into(),
            path: temp.path().to_string_lossy().into_owned(),
        };
        let session = Session {
            id: "se_attachment_cleanup".into(),
            workspace_id: workspace.id.clone(),
            title: "attachments".into(),
            branch: "main".into(),
            worktree_path: workspace.path.clone(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        let thread = Thread {
            id: "th_attachment_cleanup".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "provider/model".into(),
            model_options: serde_json::Map::new(),
            permission_mode: trouve_protocol::PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_workspace(&workspace).unwrap();
        store.insert_session(&session).unwrap();
        store
            .insert_thread(&thread, &serde_json::Map::new())
            .unwrap();
        let engine = Engine::new(store.clone(), data_dir, &Config::default());

        let removed = trouve_protocol::Attachment {
            id: "at_removed".into(),
            name: "removed.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 7,
        };
        let removed_path = attachment_dir.join("at_removed.txt");
        std::fs::write(&removed_path, "removed").unwrap();
        store
            .add_attachment(
                &thread.id,
                &removed,
                removed_path.to_string_lossy().as_ref(),
            )
            .unwrap();
        let updated = store
            .enqueue_prompt(&thread.id, "before", std::slice::from_ref(&removed))
            .unwrap();
        engine
            .update_queued_prompt(
                &updated.id,
                trouve_protocol::UpdateQueuedPromptRequest {
                    content: "after".into(),
                    retained_attachment_ids: Some(Vec::new()),
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        assert!(!removed_path.exists());
        assert!(store.attachment(&removed.id).unwrap().is_none());

        let deleted = trouve_protocol::Attachment {
            id: "at_deleted".into(),
            name: "deleted.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 7,
        };
        let deleted_path = attachment_dir.join("at_deleted.txt");
        std::fs::write(&deleted_path, "deleted").unwrap();
        store
            .add_attachment(
                &thread.id,
                &deleted,
                deleted_path.to_string_lossy().as_ref(),
            )
            .unwrap();
        let deleted_prompt = store
            .enqueue_prompt(&thread.id, "delete", std::slice::from_ref(&deleted))
            .unwrap();
        engine.delete_queued_prompt(&deleted_prompt.id).unwrap();
        assert!(!deleted_path.exists());
        assert!(store.attachment(&deleted.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn static_model_catalog_does_not_wait_for_live_discovery() {
        let data = tempfile::tempdir().unwrap();
        let live_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_provider(
            "catalog-test",
            Arc::new(CatalogTestProvider {
                live_calls: live_calls.clone(),
            }),
        );

        let static_models = engine.list_models().await;
        assert!(
            static_models
                .iter()
                .any(|model| model.id == "catalog-test/static")
        );
        assert_eq!(live_calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        let live_models = engine.refresh_models().await;
        assert!(
            live_models
                .iter()
                .any(|model| model.id == "catalog-test/live")
        );
        assert_eq!(live_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn idle_message_acceptance_persists_the_turn_shell_before_startup() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_fast_accept".into(),
            name: "fast accept".into(),
            path: data.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_fast_accept".into(),
            workspace_id: workspace.id.clone(),
            title: "Fast acceptance".into(),
            branch: "trouve/fast-accept".into(),
            worktree_path: workspace.path.clone(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_fast_accept".into(),
            session_id: session.id,
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "test/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Yolo,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let engine = Arc::new(Engine::new(
            store.clone(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        ));

        let accepted = engine
            .send_message(&thread.id, "Visible immediately".into(), Vec::new())
            .unwrap();

        assert_eq!(accepted.turn, 1);
        assert!(!accepted.queued);
        let events = store
            .events_after(&Scope::Thread(thread.id.clone()), 0)
            .unwrap();
        assert!(matches!(
            events.first().map(|event| &event.event),
            Some(Event::QueueUpdated { prompts }) if prompts.is_empty()
        ));
        assert!(matches!(
            events.get(1).map(|event| &event.event),
            Some(Event::TurnStarted { turn: 1, .. })
        ));
        assert!(matches!(
            events.get(2).map(|event| &event.event),
            Some(Event::UserMessage {
                turn: 1,
                content,
                ..
            }) if content == "Visible immediately"
        ));
    }

    #[test]
    fn active_message_acceptance_publishes_one_visible_queue_state() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_fast_queue".into(),
            name: "fast queue".into(),
            path: data.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_fast_queue".into(),
            workspace_id: workspace.id.clone(),
            title: "Fast queue".into(),
            branch: "trouve/fast-queue".into(),
            worktree_path: workspace.path,
            base_ref: "main".into(),
            archived: false,
            active: true,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_fast_queue".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "test/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Yolo,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let engine = Arc::new(Engine::new(
            store.clone(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        ));
        engine
            .active_threads
            .lock()
            .unwrap()
            .insert(thread.id.clone(), session.id);

        let accepted = engine
            .send_message(&thread.id, "Queue immediately".into(), Vec::new())
            .unwrap();

        assert_eq!(accepted.turn, 0);
        assert!(accepted.queued);
        let queue = store.queued_prompts(&thread.id).unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].content, "Queue immediately");
        assert_eq!(accepted.queued_prompt.as_ref(), queue.first());
        let events = store
            .events_after(&Scope::Thread(thread.id.clone()), 0)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.event, Event::QueueUpdated { .. }))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event.event, Event::TurnStarted { .. }))
        );
    }

    #[tokio::test]
    async fn cancellation_resolves_and_removes_pending_user_questions() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_cancel_question".into(),
            name: "cancel question".into(),
            path: data.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_cancel_question".into(),
            workspace_id: workspace.id.clone(),
            title: "Cancel question".into(),
            branch: "trouve/cancel-question".into(),
            worktree_path: workspace.path.clone(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_cancel_question".into(),
            session_id: session.id,
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "test/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Yolo,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let engine = Arc::new(Engine::new(
            store.clone(),
            data.path().into(),
            &Config::default(),
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        let waiting = tokio::spawn({
            let engine = engine.clone();
            let cancel = cancel.clone();
            let thread_id = thread.id.clone();
            async move {
                engine
                    .ask_user_questions(
                        &thread_id,
                        1,
                        "question-cancelled",
                        None,
                        Vec::new(),
                        &cancel,
                    )
                    .await
            }
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let requested = store
                    .events_after(&Scope::Thread(thread.id.clone()), 0)
                    .unwrap()
                    .iter()
                    .any(|event| matches!(event.event, Event::QuestionRequested { .. }));
                if requested {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("question should enter its pending wait");

        cancel.cancel();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .expect("question wait should stop on cancellation")
                .unwrap()
                .unwrap(),
            None
        );
        assert!(matches!(
            engine.resolve_question(&thread.id, "question-cancelled", None),
            Err(EngineError::NotFound(_))
        ));
        assert!(
            store
                .events_after(&Scope::Thread(thread.id), 0)
                .unwrap()
                .iter()
                .any(|event| matches!(event.event, Event::QuestionResolved { answers: None, .. }))
        );
    }

    #[tokio::test]
    async fn backend_collaborators_are_durable_resumable_spawned_threads() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = trouve_protocol::Workspace {
            id: "ws_collaborator".into(),
            name: "collaborator".into(),
            path: data.path().display().to_string(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_collaborator".into(),
            workspace_id: workspace.id.clone(),
            title: "Native collaborators".into(),
            branch: "trouve/collab".into(),
            worktree_path: workspace.path.clone(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let config = Config {
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine =
            Arc::new(Engine::new(store.clone(), data.path().into(), &config).with_config_dir(None));
        let parent = engine
            .create_thread(CreateThreadRequest {
                session_id: session.id.clone(),
                title: Some(session.title.clone()),
                mode: Some("code".into()),
                model: Some("codex/gpt-5.6-sol".into()),
                model_options: serde_json::Map::new(),
                permission_mode: Some(trouve_protocol::PermissionMode::Yolo),
            })
            .unwrap();
        let read_only_parent = engine
            .create_thread(CreateThreadRequest {
                session_id: session.id.clone(),
                title: Some("Read-only parent".into()),
                mode: Some("review".into()),
                model: Some("codex/gpt-5.6-sol".into()),
                model_options: serde_json::Map::new(),
                permission_mode: Some(trouve_protocol::PermissionMode::Yolo),
            })
            .unwrap();
        assert_eq!(
            engine
                .backend_collaborator_mode(
                    &session,
                    &read_only_parent,
                    BackendCollaboratorAccess::Interactive,
                )
                .unwrap(),
            "review",
            "provider metadata must not widen a read-only parent"
        );
        store
            .set_backend_session(&parent.id, "codex", "vendor-root")
            .unwrap();

        let mut vendor_threads = HashMap::from([("vendor-root".into(), parent.id.clone())]);
        let mut collaborators = HashMap::new();
        assert_eq!(
            engine
                .generate_subagent_title(None, Some("Investigate the failing test"))
                .await
                .as_deref(),
            Some("Subagent: Investigate failing test")
        );
        engine
            .start_backend_collaborator(
                &session,
                &parent,
                "codex",
                "vendor-child".into(),
                "vendor-root",
                Some("Native reviewer".into()),
                BackendCollaboratorAccess::Interactive,
                Some("Investigate the failing test".into()),
                Some("gpt-5.6-terra".into()),
                Some("high".into()),
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap();

        let child_id = vendor_threads["vendor-child"].clone();
        let child = engine.get_thread(&child_id).unwrap();
        assert!(child.spawned);
        assert_eq!(child.session_id, session.id);
        assert_eq!(child.title.as_deref(), Some("Subagent: Native reviewer"));
        assert_eq!(child.model, "codex/gpt-5.6-terra");
        assert_eq!(child.permission_mode, trouve_protocol::PermissionMode::Yolo);
        assert_eq!(
            store.spawn_parent(&child.id).unwrap(),
            Some(parent.id.clone())
        );
        assert_eq!(
            store.backend_session(&child.id, "codex").unwrap(),
            Some(("vendor-child".into(), 0))
        );
        assert_eq!(
            store.thread_model_options(&child.id).unwrap()["thinking_level"],
            "high"
        );

        // Inter-agent activity can name an ancestor as `agentThreadId`. The
        // root vendor session is already bound to the parent trouve thread and
        // must never be materialized again as a descendant of the child.
        engine
            .start_backend_collaborator(
                &session,
                &parent,
                "codex",
                "vendor-root".into(),
                "vendor-child",
                Some("root".into()),
                BackendCollaboratorAccess::Inherit,
                None,
                None,
                None,
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap();
        assert!(!collaborators.contains_key("vendor-root"));
        assert_eq!(vendor_threads["vendor-root"], parent.id);
        assert_eq!(
            engine
                .list_thread_subagents(&parent.id)
                .unwrap()
                .into_iter()
                .map(|thread| thread.id)
                .collect::<Vec<_>>(),
            vec![child.id.clone()]
        );

        let child_lifecycle = store
            .events_after(&Scope::Thread(child.id.clone()), 0)
            .unwrap();
        let capacity_cursor = child_lifecycle
            .iter()
            .find_map(|event| {
                matches!(
                    event.event,
                    Event::TurnCapacityAcquired {
                        turn: 1,
                        wait_ms: 0,
                        background: false,
                    }
                )
                .then_some(event.cursor)
            })
            .expect("native collaborator inherits the parent turn's capacity");
        let started_cursor = child_lifecycle
            .iter()
            .find_map(|event| {
                matches!(event.event, Event::TurnStarted { turn: 1, .. }).then_some(event.cursor)
            })
            .expect("native collaborator turn starts");
        assert!(capacity_cursor < started_cursor);

        // Child activity can create the projection before Codex emits its
        // formal collaborator-start notification. Publishing is tracked
        // independently from projection existence, and remains idempotent.
        assert!(
            store
                .events_after(&Scope::Thread(parent.id.clone()), 0)
                .unwrap()
                .iter()
                .all(|event| !matches!(event.event, Event::SubagentSpawned { .. }))
        );
        engine
            .publish_backend_collaborator_spawn(&parent, 1, "vendor-child", &mut collaborators)
            .await
            .unwrap();
        engine
            .publish_backend_collaborator_spawn(&parent, 1, "vendor-child", &mut collaborators)
            .await
            .unwrap();
        let parent_spawns = store
            .events_after(&Scope::Thread(parent.id.clone()), 0)
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::SubagentSpawned {
                    thread_id, prompt, ..
                } => Some((thread_id, prompt)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            parent_spawns,
            vec![(child.id.clone(), "Investigate the failing test".into())]
        );
        assert!(collaborators["vendor-child"].spawn_link_published);

        let mut claims = BackendCollaboratorClaims::new(&engine.active_threads);
        claims.claim(&child.id, &session.id);
        assert!(!engine.subagent_is_read_only(&child).unwrap());
        let accepted = engine
            .send_message(&child.id, "Follow up when finished".into(), Vec::new())
            .unwrap();
        assert!(accepted.queued);
        assert_eq!(store.queued_prompts(&child.id).unwrap().len(), 1);

        let workspace_personas = data.path().join(".agents/personas");
        std::fs::create_dir_all(&workspace_personas).unwrap();
        std::fs::write(
            workspace_personas.join("plan.toml"),
            r#"
id = "plan"
display_name = "Interactive Plan Override"
system_prompt = "This workspace intentionally made plan interactive."
allowed_tools = ["read_file"]
read_only = false
default_permission_mode = "ask"
"#,
        )
        .unwrap();
        let restricted_parent = engine
            .create_thread(CreateThreadRequest {
                session_id: session.id.clone(),
                title: Some("Restricted provider parent".into()),
                mode: Some("plan".into()),
                model: Some("codex/gpt-5.6-sol".into()),
                model_options: serde_json::Map::new(),
                permission_mode: Some(trouve_protocol::PermissionMode::Ask),
            })
            .unwrap();
        let blocked = engine
            .start_backend_collaborator(
                &session,
                &restricted_parent,
                "codex",
                "vendor-blocked".into(),
                "vendor-plan-root",
                None,
                BackendCollaboratorAccess::Inherit,
                Some("This collaborator must not start".into()),
                None,
                None,
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap_err();
        assert!(
            blocked
                .to_string()
                .contains("provider-native collaborators are not permitted")
        );
        assert!(!vendor_threads.contains_key("vendor-blocked"));
        engine
            .start_backend_collaborator(
                &session,
                &parent,
                "codex",
                "vendor-audit".into(),
                "vendor-root",
                Some("Implementation auditor".into()),
                BackendCollaboratorAccess::ReadOnly,
                Some("Audit the implementation and report issues only".into()),
                Some("gpt-5.6-terra".into()),
                None,
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap();
        let audit_child = engine.get_thread(&vendor_threads["vendor-audit"]).unwrap();
        assert!(audit_child.spawned);
        assert_eq!(audit_child.mode, "plan");
        assert!(engine.subagent_is_read_only(&audit_child).unwrap());
        assert!(matches!(
            engine.send_message(&audit_child.id, "Make a follow-up change".into(), Vec::new()),
            Err(EngineError::Conflict(message))
                if message.contains("read-only exploration, audit, or review mode")
        ));
        assert!(store.queued_prompts(&audit_child.id).unwrap().is_empty());

        let mode = personas::find_persona(&personas::builtin_personas(), "code")
            .unwrap()
            .clone();
        std::fs::write(data.path().join("bridge-owner.txt"), "owned by the child").unwrap();
        let bridge_arguments = serde_json::json!({ "path": "bridge-owner.txt" });
        let bridge_wrapper = serde_json::json!({
            "type": "mcpToolCall",
            "server": "trouve",
            "tool": "read_file",
            "arguments": bridge_arguments.clone()
        });
        let root_cancel = engine.register_cancel(&parent.id);
        engine
            .bridged_tool_owners
            .bind_vendor_thread(&parent.id, "vendor-root", &parent.id)
            .unwrap();
        engine
            .bridged_tool_owners
            .bind_vendor_thread(&parent.id, "vendor-child", &child.id)
            .unwrap();
        engine
            .bridged_tool_owners
            .bind_vendor_thread(&parent.id, "vendor-audit", &audit_child.id)
            .unwrap();
        let missing_owner = engine
            .bridged_codex_tool_call(&parent.id, None, None, "read_file", &bridge_arguments)
            .await
            .unwrap_err();
        assert!(missing_owner.to_string().contains("_meta.threadId"));
        let unknown_owner = engine
            .bridged_codex_tool_call(
                &parent.id,
                Some("vendor-external"),
                None,
                "read_file",
                &bridge_arguments,
            )
            .await
            .unwrap_err();
        assert!(unknown_owner.to_string().contains("unknown, external"));
        let thread_only_output = engine
            .bridged_codex_tool_call(
                &parent.id,
                Some("vendor-audit"),
                None,
                "read_file",
                &bridge_arguments,
            )
            .await
            .unwrap();
        assert!(thread_only_output.contains("owned by the child"));
        {
            let projection = collaborators.get_mut("vendor-child").unwrap();
            assert!(engine.suppress_collaborator_bridge_wrapper(
                &parent.id,
                "vendor-child",
                projection,
                &BackendCollaboratorEvent::ToolStarted {
                    call_id: "codex-wrapper".into(),
                    tool: "mcpToolCall".into(),
                    args: bridge_wrapper,
                },
            ));
        }
        let bridge_output = engine
            .bridged_codex_tool_call(
                &parent.id,
                Some("vendor-child"),
                Some("codex-wrapper"),
                "read_file",
                &bridge_arguments,
            )
            .await
            .unwrap();
        assert!(bridge_output.contains("owned by the child"));
        {
            let projection = collaborators.get_mut("vendor-child").unwrap();
            assert!(engine.suppress_collaborator_bridge_wrapper(
                &parent.id,
                "vendor-child",
                projection,
                &BackendCollaboratorEvent::ToolCompleted {
                    call_id: "codex-wrapper".into(),
                    ok: true,
                    result: serde_json::json!({ "status": "completed" }),
                },
            ));
        }
        let child_bridge_events = store
            .events_after(&Scope::Thread(child.id.clone()), 0)
            .unwrap();
        assert_eq!(
            child_bridge_events
                .iter()
                .filter(|event| matches!(
                    &event.event,
                    Event::ToolRequested { tool, .. } if tool == "read_file"
                ))
                .count(),
            1
        );
        assert!(child_bridge_events.iter().all(|event| !matches!(
            &event.event,
            Event::ToolRequested { tool, .. } if tool == "mcpToolCall"
        )));
        assert!(
            store
                .events_after(&Scope::Thread(parent.id.clone()), 0)
                .unwrap()
                .iter()
                .all(|event| !matches!(
                    &event.event,
                    Event::ToolRequested { tool, .. } if tool == "read_file"
                ))
        );

        // Two collaborators submit byte-identical mutations while the HTTP
        // and wrapper transports arrive in opposite orders. Explicit vendor
        // metadata sends each call through its own mode policy: the code
        // child writes, while the review child is denied.
        let identical_write_args = serde_json::json!({
            "path": "metadata-owner.txt",
            "content": "written by interactive child"
        });
        let write_wrapper = serde_json::json!({
            "type": "mcpToolCall",
            "server": "trouve",
            "tool": "write_file",
            "arguments": identical_write_args.clone()
        });
        let request_first = tokio::spawn({
            let engine = engine.clone();
            let parent_id = parent.id.clone();
            let arguments = identical_write_args.clone();
            async move {
                engine
                    .bridged_codex_tool_call(
                        &parent_id,
                        Some("vendor-child"),
                        Some("request-first-write"),
                        "write_file",
                        &arguments,
                    )
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let pending = engine
                    .bridged_tool_owners
                    .state
                    .lock()
                    .unwrap()
                    .roots
                    .get(&parent.id)
                    .is_some_and(|root| root.pending_calls.contains_key("request-first-write"));
                if pending {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("HTTP-first call should enter identity rendezvous");
        {
            let projection = collaborators.get_mut("vendor-child").unwrap();
            assert!(engine.suppress_collaborator_bridge_wrapper(
                &parent.id,
                "vendor-child",
                projection,
                &BackendCollaboratorEvent::ToolStarted {
                    call_id: "request-first-write".into(),
                    tool: "mcpToolCall".into(),
                    args: write_wrapper.clone(),
                },
            ));
        }
        request_first.await.unwrap().unwrap();

        {
            let projection = collaborators.get_mut("vendor-audit").unwrap();
            assert!(engine.suppress_collaborator_bridge_wrapper(
                &parent.id,
                "vendor-audit",
                projection,
                &BackendCollaboratorEvent::ToolStarted {
                    call_id: "wrapper-first-write".into(),
                    tool: "mcpToolCall".into(),
                    args: write_wrapper,
                },
            ));
        }
        let review_result = engine
            .bridged_codex_tool_call(
                &parent.id,
                Some("vendor-audit"),
                Some("wrapper-first-write"),
                "write_file",
                &identical_write_args,
            )
            .await
            .unwrap();
        assert!(review_result.contains("not permitted in this mode"));
        assert_eq!(
            std::fs::read_to_string(data.path().join("metadata-owner.txt")).unwrap(),
            "written by interactive child"
        );

        let projection = collaborators.get_mut("vendor-child").unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::ThinkingDelta("Checking the suite.".into()),
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::ThinkingCompleted,
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::ToolStarted {
                    call_id: "call-1".into(),
                    tool: "shell".into(),
                    args: serde_json::json!({ "command": "cargo test" }),
                },
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::ToolOutput {
                    call_id: "call-1".into(),
                    chunk: "all green".into(),
                },
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::ToolCompleted {
                    call_id: "call-1".into(),
                    ok: true,
                    result: serde_json::json!({ "exit_code": 0 }),
                },
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::TextDelta("The suite passes.".into()),
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::Completed {
                    usage: Usage {
                        input_tokens: 7,
                        cached_input_tokens: 2,
                        output_tokens: 3,
                        ..Usage::default()
                    },
                },
                &root_cancel,
            )
            .await
            .unwrap();
        assert!(projection.terminal);
        assert_eq!(
            store.backend_session(&child.id, "codex").unwrap(),
            Some(("vendor-child".into(), 2))
        );

        let events = store
            .events_after(&Scope::Thread(child.id.clone()), 0)
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| matches!(event.event, Event::TurnStarted { turn: 1, .. }))
        );
        assert!(events.iter().any(|event| matches!(
            &event.event,
            Event::UserMessage { turn: 1, content, .. }
                if content == "Investigate the failing test"
        )));
        assert!(events.iter().any(|event| matches!(
            &event.event,
            Event::AssistantThinking { turn: 1, text }
                if text == "Checking the suite."
        )));
        assert!(events.iter().any(|event| matches!(
            &event.event,
            Event::ToolCompleted { call_id, status: ToolStatus::Ok, .. }
                if call_id == "call-1"
        )));
        assert!(events.iter().any(|event| matches!(
            &event.event,
            Event::AssistantMessage { turn: 1, content }
                if content == "The suite passes."
        )));
        assert!(events.iter().any(|event| matches!(
            event.event,
            Event::TurnCompleted {
                turn: 1,
                checkpoint_id: None,
                ..
            }
        )));
        claims.release(&child.id);

        // Codex recovers prompts asynchronously when a collaborator
        // announcement only names the child thread. A fast child can finish
        // before that lookup returns; the recovered prompt must still become
        // the turn's durable Prompt node instead of being dropped because the
        // collaborator is already terminal.
        engine
            .start_backend_collaborator(
                &session,
                &parent,
                "codex",
                "vendor-late-prompt".into(),
                "vendor-root",
                None,
                BackendCollaboratorAccess::Inherit,
                None,
                None,
                None,
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap();
        let late_prompt_child_id = vendor_threads["vendor-late-prompt"].clone();
        engine
            .publish_backend_collaborator_spawn(
                &parent,
                1,
                "vendor-late-prompt",
                &mut collaborators,
            )
            .await
            .unwrap();
        assert!(
            store
                .events_after(&Scope::Thread(parent.id.clone()), 0)
                .unwrap()
                .iter()
                .all(|event| !matches!(
                    &event.event,
                    Event::SubagentSpawned { thread_id, .. }
                        if thread_id == &late_prompt_child_id
                )),
            "an unresolved prompt must not publish an empty parent card"
        );
        {
            let projection = collaborators.get_mut("vendor-late-prompt").unwrap();
            engine
                .persist_backend_collaborator_event(
                    &session,
                    &mode,
                    "codex",
                    projection,
                    BackendCollaboratorEvent::ThinkingDelta("Finishing quickly.".into()),
                    &root_cancel,
                )
                .await
                .unwrap();
            engine
                .persist_backend_collaborator_event(
                    &session,
                    &mode,
                    "codex",
                    projection,
                    BackendCollaboratorEvent::Completed {
                        usage: Usage::default(),
                    },
                    &root_cancel,
                )
                .await
                .unwrap();
            assert!(projection.terminal);
            engine
                .persist_backend_collaborator_event(
                    &session,
                    &mode,
                    "codex",
                    projection,
                    BackendCollaboratorEvent::UserMessage("Recovered after completion.".into()),
                    &root_cancel,
                )
                .await
                .unwrap();
            // A repeated recovery notification remains idempotent.
            engine
                .persist_backend_collaborator_event(
                    &session,
                    &mode,
                    "codex",
                    projection,
                    BackendCollaboratorEvent::UserMessage("Recovered after completion.".into()),
                    &root_cancel,
                )
                .await
                .unwrap();
        }
        engine
            .publish_backend_collaborator_spawn(
                &parent,
                1,
                "vendor-late-prompt",
                &mut collaborators,
            )
            .await
            .unwrap();
        engine
            .publish_backend_collaborator_spawn(
                &parent,
                1,
                "vendor-late-prompt",
                &mut collaborators,
            )
            .await
            .unwrap();
        let late_parent_prompts = store
            .events_after(&Scope::Thread(parent.id.clone()), 0)
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::SubagentSpawned {
                    thread_id, prompt, ..
                } if thread_id == late_prompt_child_id => Some(prompt),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            late_parent_prompts,
            vec!["Recovered after completion.".to_string()]
        );
        assert!(collaborators["vendor-late-prompt"].spawn_link_published);
        let late_prompt_events = store
            .events_after(&Scope::Thread(late_prompt_child_id.clone()), 0)
            .unwrap();
        let completed_cursor = late_prompt_events
            .iter()
            .find_map(|event| {
                matches!(event.event, Event::TurnCompleted { turn: 1, .. }).then_some(event.cursor)
            })
            .unwrap();
        let recovered_prompts = late_prompt_events
            .iter()
            .filter_map(|event| match &event.event {
                Event::UserMessage {
                    turn: 1, content, ..
                } => Some((event.cursor, content.as_str())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(recovered_prompts.len(), 1);
        assert_eq!(recovered_prompts[0].1, "Recovered after completion.");
        assert!(recovered_prompts[0].0 > completed_cursor);
        let provider_messages = store
            .messages(&late_prompt_child_id)
            .unwrap()
            .into_iter()
            .map(serde_json::from_value::<Message>)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(matches!(
            provider_messages.as_slice(),
            [Message::Assistant { content, .. }] if content.is_empty()
        ));
        assert_eq!(
            store
                .backend_session(&late_prompt_child_id, "codex")
                .unwrap(),
            Some(("vendor-late-prompt".into(), 1))
        );

        engine
            .start_backend_collaborator(
                &session,
                &parent,
                "codex",
                "vendor-child".into(),
                "vendor-root",
                None,
                BackendCollaboratorAccess::Inherit,
                Some("Run a follow-up check".into()),
                None,
                None,
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap();
        let projection = collaborators.get_mut("vendor-child").unwrap();
        engine
            .prepare_backend_collaborator_turn(
                &session,
                "codex",
                projection,
                Some("vendor-child-turn-2"),
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::TurnStarted,
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .start_backend_collaborator(
                &session,
                &parent,
                "codex",
                "vendor-child".into(),
                "vendor-root",
                None,
                BackendCollaboratorAccess::Inherit,
                Some("Also inspect the current protocol".into()),
                None,
                None,
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap();
        let projection = collaborators.get_mut("vendor-child").unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::UserMessage("Also inspect the current protocol".into()),
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::TextDelta("The follow-up passes.".into()),
                &root_cancel,
            )
            .await
            .unwrap();
        engine
            .persist_backend_collaborator_event(
                &session,
                &mode,
                "codex",
                projection,
                BackendCollaboratorEvent::Completed {
                    usage: Usage::default(),
                },
                &root_cancel,
            )
            .await
            .unwrap();
        let events = store
            .events_after(&Scope::Thread(child.id.clone()), 0)
            .unwrap();
        assert!(events.iter().any(|event| matches!(
            &event.event,
            Event::UserMessage { turn: 2, content, .. } if content == "Run a follow-up check"
        )));
        assert!(events.iter().any(|event| matches!(
            &event.event,
            Event::TurnSteered { turn: 2, content, .. }
                if content == "Also inspect the current protocol"
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.event, Event::TurnCompleted { .. }))
                .count(),
            2
        );
        assert_eq!(
            store.backend_session(&child.id, "codex").unwrap(),
            Some(("vendor-child".into(), 5))
        );

        engine
            .start_backend_collaborator(
                &session,
                &parent,
                "codex",
                "vendor-grandchild".into(),
                "vendor-child",
                None,
                BackendCollaboratorAccess::Inherit,
                Some("Verify one more edge case".into()),
                None,
                None,
                &mut vendor_threads,
                &mut collaborators,
            )
            .await
            .unwrap();
        let grandchild_id = vendor_threads["vendor-grandchild"].clone();
        assert_eq!(
            store.spawn_parent(&grandchild_id).unwrap(),
            Some(child.id.clone())
        );
        let grandchild = engine.get_thread(&grandchild_id).unwrap();
        assert!(grandchild.spawned);
        assert_eq!(grandchild.model, "codex/gpt-5.6-terra");
        assert_eq!(
            store.thread_model_options(&grandchild.id).unwrap()["thinking_level"],
            "high"
        );
        let grandchild_events = store
            .events_after(&Scope::Thread(grandchild.id.clone()), 0)
            .unwrap();
        let grandchild_capacity = grandchild_events
            .iter()
            .position(|event| {
                matches!(
                    event.event,
                    Event::TurnCapacityAcquired {
                        turn: 1,
                        wait_ms: 0,
                        background: false,
                    }
                )
            })
            .expect("nested collaborator inherits capacity");
        let grandchild_started = grandchild_events
            .iter()
            .position(|event| matches!(event.event, Event::TurnStarted { turn: 1, .. }))
            .expect("nested collaborator starts");
        assert!(grandchild_capacity < grandchild_started);
        engine
            .publish_backend_collaborator_spawn(&parent, 1, "vendor-grandchild", &mut collaborators)
            .await
            .unwrap();
        assert!(
            store
                .events_after(&Scope::Thread(child.id.clone()), 0)
                .unwrap()
                .iter()
                .any(|event| matches!(
                    &event.event,
                    Event::SubagentSpawned {
                        turn: 2,
                        thread_id,
                        prompt,
                        ..
                    } if thread_id == &grandchild.id && prompt == "Verify one more edge case"
                ))
        );
        let direct_children = engine.list_thread_subagents(&parent.id).unwrap();
        assert!(direct_children.iter().any(|thread| thread.id == child.id));
        assert!(
            direct_children
                .iter()
                .any(|thread| thread.id == audit_child.id)
        );
        assert!(
            direct_children
                .iter()
                .all(|thread| thread.id != grandchild.id),
            "the existing subagents API remains direct-child-only"
        );
        let descendants = engine.list_thread_descendants(&parent.id).unwrap();
        assert!(descendants.iter().any(|thread| thread.id == child.id));
        assert!(descendants.iter().any(|thread| thread.id == audit_child.id));
        assert!(descendants.iter().any(|thread| thread.id == grandchild.id));
        assert!(engine.thread_can_spawn_subagents(&grandchild.id).unwrap());

        let mut deepest = grandchild;
        for title in ["Great grandchild", "Great great grandchild"] {
            let nested = engine
                .create_thread(CreateThreadRequest {
                    session_id: session.id.clone(),
                    title: Some(title.into()),
                    mode: Some("code".into()),
                    model: Some("codex/gpt-5.6-terra".into()),
                    model_options: serde_json::Map::new(),
                    permission_mode: Some(trouve_protocol::PermissionMode::Yolo),
                })
                .unwrap();
            store
                .insert_spawned(&nested.id, &deepest.id, "thread")
                .unwrap();
            deepest = nested;
        }
        assert_eq!(
            engine.subagent_root_and_depth(&deepest.id).unwrap(),
            (parent.id.clone(), MAX_SUBAGENT_DEPTH)
        );
        assert!(!engine.thread_can_spawn_subagents(&deepest.id).unwrap());

        let cancelled_call = tokio::spawn({
            let engine = engine.clone();
            let parent_id = parent.id.clone();
            async move {
                engine
                    .bridged_codex_tool_call(
                        &parent_id,
                        Some("vendor-child"),
                        Some("cancelled-metadata-call"),
                        "read_file",
                        &serde_json::json!({ "path": "bridge-owner.txt" }),
                    )
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let pending = engine
                    .bridged_tool_owners
                    .state
                    .lock()
                    .unwrap()
                    .roots
                    .get(&parent.id)
                    .is_some_and(|root| root.pending_calls.contains_key("cancelled-metadata-call"));
                if pending {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("call should wait for its wrapper before root cancellation");
        root_cancel.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), cancelled_call)
                .await
                .expect("root cancellation should stop metadata rendezvous")
                .unwrap(),
            Err(EngineError::Conflict(message)) if message.contains("cancelled")
        ));
        engine.clear_cancel(&parent.id);
    }

    fn projection_pr(number: u64, workspace_id: &str, head: &str) -> trouve_protocol::PrInfo {
        trouve_protocol::PrInfo {
            host: "github.com".into(),
            repository: "acme/widgets".into(),
            workspace_id: workspace_id.into(),
            number,
            url: format!("https://github.com/acme/widgets/pull/{number}"),
            title: format!("Pull request {number}"),
            state: "open".into(),
            draft: false,
            base: "main".into(),
            head: head.into(),
            head_sha: None,
            checks: Vec::new(),
            reviews: Vec::new(),
            trouve_review: None,
            author: "octocat".into(),
            requested_reviewers: Vec::new(),
            comments: 0,
            last_comment_at: None,
            mergeable: None,
            merge_state_status: None,
            merged_at: None,
        }
    }

    #[test]
    fn pr_verification_requires_exact_session_head_and_head_repository() {
        let mut info = projection_pr(42, "ws", "agent/clean-pr");
        info.head_sha = Some("1".repeat(40));
        let intent = SessionPrVerificationIntent {
            session_id: "se_1".into(),
            host: "github.com".into(),
            owner: "acme".into(),
            repository: "widgets".into(),
            number: 42,
            branch: "agent/clean-pr".into(),
            head_sha: "1".repeat(40),
            attempts: 0,
            last_failure_class: String::new(),
            consecutive_failures: 0,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let mut fetched = crate::github::PullRequestWithHeadRepository {
            info,
            head_repository: Some("acme/widgets".into()),
        };

        assert!(Engine::pr_matches_verification_intent(&fetched, &intent));
        fetched.head_repository = Some("attacker/widgets".into());
        assert!(!Engine::pr_matches_verification_intent(&fetched, &intent));
        fetched.head_repository = Some("acme/widgets".into());
        fetched.info.head = "other-session".into();
        assert!(!Engine::pr_matches_verification_intent(&fetched, &intent));
        fetched.info.head = intent.branch.clone();
        fetched.info.head_sha = Some("2".repeat(40));
        assert!(Engine::pr_repository_and_branch_match(&fetched, &intent));
        assert!(!Engine::pr_matches_verification_intent(&fetched, &intent));

        assert!(!Engine::session_pr_verification_expired(&intent));
        let expired = SessionPrVerificationIntent {
            created_at: (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339(),
            ..intent.clone()
        };
        assert!(Engine::session_pr_verification_expired(&expired));
        let malformed = SessionPrVerificationIntent {
            created_at: "not-a-timestamp".into(),
            ..intent.clone()
        };
        assert!(Engine::session_pr_verification_expired(&malformed));

        let accumulated_transient = SessionPrVerificationIntent {
            attempts: 7,
            last_failure_class: PR_VERIFICATION_FAILURE_TRANSIENT.into(),
            consecutive_failures: 7,
            ..intent.clone()
        };
        assert!(!Engine::session_pr_verification_retry_exhausted(
            &accumulated_transient,
            PR_VERIFICATION_FAILURE_NOT_FOUND,
            Some(MAX_SESSION_PR_NOT_FOUND_ATTEMPTS),
            true,
        ));
        assert_eq!(
            Engine::session_pr_verification_retry_delay(
                &accumulated_transient,
                PR_VERIFICATION_FAILURE_NOT_FOUND,
                true,
            ),
            128,
            "request backoff must not reset when the failure class changes",
        );
        let consecutive_missing = SessionPrVerificationIntent {
            last_failure_class: PR_VERIFICATION_FAILURE_NOT_FOUND.into(),
            consecutive_failures: MAX_SESSION_PR_NOT_FOUND_ATTEMPTS - 1,
            ..accumulated_transient.clone()
        };
        assert!(Engine::session_pr_verification_retry_exhausted(
            &consecutive_missing,
            PR_VERIFICATION_FAILURE_NOT_FOUND,
            Some(MAX_SESSION_PR_NOT_FOUND_ATTEMPTS),
            true,
        ));
        let request_ceiling = SessionPrVerificationIntent {
            attempts: MAX_SESSION_PR_REQUEST_ATTEMPTS - 1,
            ..accumulated_transient.clone()
        };
        assert!(Engine::session_pr_verification_retry_exhausted(
            &request_ceiling,
            PR_VERIFICATION_FAILURE_TRANSIENT,
            None,
            true,
        ));
        assert!(!Engine::session_pr_verification_retry_exhausted(
            &request_ceiling,
            PR_VERIFICATION_FAILURE_AUTH,
            None,
            false,
        ));
        assert_eq!(
            Engine::session_pr_verification_retry_delay(
                &request_ceiling,
                PR_VERIFICATION_FAILURE_AUTH,
                false,
            ),
            SESSION_PR_AUTH_RETRY_SECONDS,
        );
        let repeated_auth = SessionPrVerificationIntent {
            last_failure_class: PR_VERIFICATION_FAILURE_AUTH.into(),
            consecutive_failures: 10,
            ..request_ceiling.clone()
        };
        assert_eq!(
            Engine::session_pr_verification_retry_delay(
                &repeated_auth,
                PR_VERIFICATION_FAILURE_AUTH,
                false,
            ),
            21_600,
            "authentication backoff must grow and cap at six hours",
        );
        assert!(!Engine::session_pr_verification_retry_exhausted(
            &accumulated_transient,
            PR_VERIFICATION_FAILURE_CONTENTION,
            None,
            false,
        ));
    }

    #[test]
    fn pr_verification_nominations_are_bounded_per_creator_call() {
        let session = Session {
            id: "se_bounded_prs".into(),
            workspace_id: "ws".into(),
            title: "Bounded PRs".into(),
            branch: "agent/bounded".into(),
            worktree_path: "/tmp/bounded-prs".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        let intents = Engine::session_pr_verification_intents(
            &session,
            ("github.com".into(), "acme".into(), "widgets".into()),
            [4, 2],
            1..=(MAX_SESSION_PR_VERIFICATIONS_PER_CREATION_CALL as u64 + 5),
            Some((session.branch.clone(), "1".repeat(40))),
        );

        assert_eq!(
            intents.len(),
            MAX_SESSION_PR_VERIFICATIONS_PER_CREATION_CALL
        );
        assert_eq!(
            intents
                .iter()
                .map(|intent| intent.number)
                .collect::<Vec<_>>(),
            [4, 2].into_iter().chain((8..=21).rev()).collect::<Vec<_>>()
        );
        let missing_evidence = Engine::session_pr_verification_intents(
            &session,
            ("github.com".into(), "acme".into(), "widgets".into()),
            [99],
            Vec::new(),
            None,
        );
        assert!(missing_evidence.is_empty());
        let wrong_branch = Engine::session_pr_verification_intents(
            &session,
            ("github.com".into(), "acme".into(), "widgets".into()),
            [42],
            [],
            Some(("agent/other-session".into(), "1".repeat(40))),
        );
        assert!(wrong_branch.is_empty());
    }

    fn projection_detail(info: trouve_protocol::PrInfo) -> trouve_protocol::PrDetail {
        serde_json::from_value(serde_json::json!({
            "info": info,
            "base_sha": "base-sha",
            "id": "PR_node",
            "viewer": "octocat",
            "created_at": "2026-08-08T00:00:00Z",
            "updated_at": "2026-08-08T00:00:00Z",
            "additions": 1,
            "deletions": 1,
            "changed_files": 1,
            "commit_count": 1,
            "capabilities": {},
            "merge_queue": { "enabled": false }
        }))
        .unwrap()
    }

    #[test]
    fn pr_detail_cache_merges_lazy_sections_and_invalidates_independently() {
        use trouve_protocol::PrDetailSection as Section;

        let mut info = projection_pr(42, "ws", "trouve/cache");
        info.head_sha = Some("head-one".into());
        let key = GithubPrDetailKey::from_info(&info);
        let mut cache = GithubPrDetailCache::default();
        cache.merge(
            &key,
            projection_detail(info.clone()),
            HashSet::from([Section::Overview]),
        );

        let mut files = projection_detail(info.clone());
        files.files.push(trouve_protocol::PrFile {
            path: "src/lib.rs".into(),
            additions: 1,
            deletions: 1,
            change_type: "modified".into(),
            viewer_viewed_state: "unviewed".into(),
        });
        cache.merge(&key, files, HashSet::from([Section::Files]));
        let cached = cache
            .get(&key, &HashSet::from([Section::Overview, Section::Files]))
            .unwrap();
        assert_eq!(cached.files.len(), 1);

        cache.mark_stale(&key, &HashSet::from([Section::Files]));
        assert!(cache.get(&key, &HashSet::from([Section::Files])).is_none());
        assert!(
            cache
                .get(&key, &HashSet::from([Section::Overview]))
                .is_some()
        );

        let mut next = info;
        next.head_sha = Some("head-two".into());
        let next_detail = cache.merge(
            &key,
            projection_detail(next),
            HashSet::from([Section::Overview]),
        );
        assert!(next_detail.files.is_empty());
        assert!(!cache.entries.contains_key(&key));
    }

    struct BlockingProviderSecretStore {
        values: Mutex<HashMap<String, String>>,
        delete_started: std::sync::Barrier,
        allow_delete: std::sync::Barrier,
    }

    impl BlockingProviderSecretStore {
        fn new() -> Self {
            Self {
                values: Mutex::new(HashMap::new()),
                delete_started: std::sync::Barrier::new(2),
                allow_delete: std::sync::Barrier::new(2),
            }
        }
    }

    impl trouve_providers::secrets::SecretStore for BlockingProviderSecretStore {
        fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
            Ok(self.values.lock().unwrap().get(key).cloned())
        }

        fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
            self.values
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, key: &str) -> anyhow::Result<()> {
            if key == trouve_providers::secrets::api_key_secret("serialized") {
                self.delete_started.wait();
                self.allow_delete.wait();
            }
            self.values.lock().unwrap().remove(key);
            Ok(())
        }
    }

    #[tokio::test]
    async fn removing_github_host_discards_its_dashboard_cache() {
        const HOST: &str = "github.example.com";

        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let config = Config {
            local_enabled: Some(false),
            github_enterprise: vec![crate::config::GithubEnterpriseConfig {
                host: HOST.into(),
                client_id: Some("client-id".into()),
            }],
            ..Default::default()
        };
        let engine = Engine::new(store.clone(), data.path().into(), &config);
        let mut cache = crate::github::GitHubDashboardCache::default();
        cache.mark_snapshot_published("stale snapshot".into());
        let cache_handle = Arc::new(tokio::sync::Mutex::new(cache));
        engine
            .github_dashboard_caches
            .lock()
            .unwrap()
            .insert(HOST.into(), cache_handle.clone());
        let in_flight = cache_handle.lock().await;

        engine.remove_github_host(HOST).unwrap();

        assert!(
            !engine
                .github_dashboard_caches
                .lock()
                .unwrap()
                .contains_key(HOST)
        );
        drop(in_flight);
        let cleared = store.latest_github_pr_snapshot(HOST).unwrap().unwrap();
        assert!(cleared.viewer.is_empty());
        assert!(cleared.prs.is_empty());

        engine.add_github_host(HOST, "client-id").unwrap();
        let cache = {
            let mut caches = engine.github_dashboard_caches.lock().unwrap();
            caches
                .entry(HOST.into())
                .or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(
                        crate::github::GitHubDashboardCache::default(),
                    ))
                })
                .clone()
        };
        let cache = cache.lock().await;
        assert!(!cache.has_published_snapshot());
    }

    #[test]
    fn server_projection_bootstraps_branch_and_recorded_session_prs_locally() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .insert_workspace(&trouve_protocol::Workspace {
                id: "ws_projection".into(),
                name: "projection".into(),
                path: "/tmp/projection".into(),
            })
            .unwrap();
        let session = Session {
            id: "se_projection".into(),
            workspace_id: "ws_projection".into(),
            title: "Projection".into(),
            branch: "trouve/exact".into(),
            worktree_path: "/tmp/projection".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let exact = projection_pr(10, &session.workspace_id, &session.branch);
        let linked = projection_pr(11, &session.workspace_id, "external-branch");
        store
            .append_event(
                Scope::Server,
                Event::GithubPullRequestsUpdated {
                    pull_requests: trouve_protocol::GithubPrList {
                        viewer: "octocat".into(),
                        host: "github.com".into(),
                        prs: vec![exact, linked.clone()],
                    },
                },
            )
            .unwrap();
        store
            .append_event(
                Scope::Session(session.id.clone()),
                Event::SessionPrOpened {
                    number: linked.number,
                    url: linked.url,
                },
            )
            .unwrap();
        let engine = Engine::new(store, data.path().into(), &Config::default());

        let local = engine.projected_session_prs(&session.id).unwrap();
        assert_eq!(
            local.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![11, 10]
        );

        let (cursor, projection) = engine.server_projection_snapshot().unwrap();

        assert!(cursor > 0);
        assert_eq!(projection.github_pull_requests.len(), 1);
        assert_eq!(projection.github_pull_requests[0].cursor, cursor);
        assert_eq!(projection.session_pull_requests.len(), 1);
        assert_eq!(projection.session_pull_requests[0].session_id, session.id);
        assert_eq!(
            projection.session_pull_requests[0]
                .prs
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<_>>(),
            vec![11, 10]
        );
    }

    #[test]
    fn stale_github_refresh_registration_cannot_survive_host_removal() {
        const HOST: &str = "github.example.com";

        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let config = Config {
            local_enabled: Some(false),
            github_enterprise: vec![crate::config::GithubEnterpriseConfig {
                host: HOST.into(),
                client_id: Some("client-id".into()),
            }],
            ..Default::default()
        };
        let mut engine = Engine::new(store.clone(), data.path().into(), &config);
        engine.secrets = Arc::new(trouve_providers::secrets::FileStore::new(
            data.path().join("secrets.json"),
        ));
        let tokens = trouve_providers::auth::OAuthTokens {
            access_token: "token".into(),
            refresh_token: None,
            expires_at: None,
            id_token: None,
        };
        engine
            .secrets
            .set(
                &trouve_providers::secrets::oauth_secret(&Engine::github_secret_id(HOST)),
                &serde_json::to_string(&tokens).unwrap(),
            )
            .unwrap();
        let engine = Arc::new(engine);
        let (captured_tx, captured_rx) = std::sync::mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);

        let refresh_engine = Arc::clone(&engine);
        let refresh = std::thread::spawn(move || {
            refresh_engine.prepare_github_dashboard_refreshes_with(|| {
                captured_tx.send(()).unwrap();
                resume_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            })
        });
        captured_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(
            engine.github_dashboard_publication.try_lock().is_err(),
            "host capture and cache registration must share the publication lock"
        );

        let removal_engine = Arc::clone(&engine);
        let removal = std::thread::spawn(move || removal_engine.remove_github_host(HOST));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while engine
            .github_hosts()
            .iter()
            .any(|(host, _client_id)| host == HOST)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "host removal did not update config"
            );
            std::thread::yield_now();
        }
        assert!(
            !removal.is_finished(),
            "removal must wait for refresh registration before clearing it"
        );

        resume_tx.send(()).unwrap();
        let refreshes = refresh.join().unwrap();
        removal.join().unwrap().unwrap();

        let (_, _, stale_cache) = refreshes.into_iter().next().unwrap();
        let cache_is_current = engine
            .github_dashboard_caches
            .lock()
            .unwrap()
            .get(HOST)
            .is_some_and(|current| Arc::ptr_eq(current, &stale_cache));
        assert!(!cache_is_current);
        let cleared = store.latest_github_pr_snapshot(HOST).unwrap().unwrap();
        assert!(cleared.viewer.is_empty());
        assert!(cleared.prs.is_empty());
    }

    #[test]
    fn collects_provider_neutral_pr_evidence() {
        let remote_commit = "9f2c6d8b18c86d48ca2c3f58191f9f5277b9269a";
        let branch_args = serde_json::json!({
            "request": {
                "method": "POST",
                "url": "https://api.github.com/repos/o/r/git/refs",
                "body": {"ref": "refs/heads/fix/manual-pr"}
            }
        });
        let structured_pr = serde_json::json!({
            "data": {"createPullRequest": {"pullRequest": {
                "url": "https://github.com/o/r/pull/75"
            }}},
            "unrelated": "https://github.com/elsewhere/r/pull/99",
        });
        assert_eq!(
            pr_numbers_in_value(&structured_pr, "github.com", "o", "r"),
            HashSet::from([75])
        );

        let events = vec![
            Event::ToolRequested {
                turn: 1,
                call_id: "branch".into(),
                tool: "github_rest".into(),
                args: branch_args.clone(),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "branch".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "ref": "refs/heads/fix/manual-pr",
                    "object": {"sha": remote_commit}
                }),
                execution_duration_ms: None,
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "failed".into(),
                tool: "shell".into(),
                args: serde_json::json!({"cmd": "gh pr create --head fix/failed"}),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "failed".into(),
                chunk: "https://github.com/o/r/pull/76".into(),
            },
            Event::ToolCompleted {
                call_id: "failed".into(),
                status: ToolStatus::Error,
                result: serde_json::Value::Null,
                execution_duration_ms: None,
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "graphql".into(),
                tool: "github_graphql".into(),
                args: serde_json::json!({
                    "query": "mutation { createPullRequest(input: $input) { pullRequest { url } } }"
                }),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "graphql".into(),
                chunk: structured_pr.to_string(),
            },
            Event::ToolCompleted {
                call_id: "graphql".into(),
                status: ToolStatus::Ok,
                result: serde_json::Value::Null,
                execution_duration_ms: None,
            },
            // Successful list/view output may mention many PRs, but none of
            // them were created by this session.
            Event::ToolRequested {
                turn: 1,
                call_id: "list".into(),
                tool: "shell".into(),
                args: serde_json::json!({"cmd": "gh pr list --json url"}),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "list".into(),
                chunk: "https://github.com/o/r/pull/74".into(),
            },
            Event::ToolCompleted {
                call_id: "list".into(),
                status: ToolStatus::Ok,
                result: serde_json::Value::Null,
                execution_duration_ms: None,
            },
            Event::UserMessage {
                turn: 2,
                content: "Please compare with repos/o/r/pulls/73".into(),
                attachments: vec![],
                background: false,
            },
        ];
        let evidence = pr_evidence_from_events(events, "github.com", "o", "r");
        assert_eq!(evidence.numbers, HashSet::from([75]));
        assert_eq!(evidence.successful_tool_args.len(), 2);
        assert!(
            evidence
                .successful_tool_args
                .iter()
                .any(|args| args.contains("fix/manual-pr"))
        );
        assert!(
            evidence
                .successful_tool_args
                .iter()
                .all(|args| !args.contains("fix/failed"))
        );
        assert_eq!(evidence.commit_ids, HashSet::from([remote_commit.into()]));
    }

    #[test]
    fn recognizes_creation_without_treating_pr_reads_as_associations() {
        assert!(could_request_pull_request_creation(
            "mcpToolCall",
            &serde_json::json!({
                "server": "codex_apps",
                "tool": "github.create_pull_request",
                "arguments": "{undecodable"
            })
        ));
        assert!(requests_pull_request_creation(
            "mcpToolCall",
            &serde_json::json!({
                "type": "mcpToolCall",
                "server": "codex_apps",
                "tool": "github.create_pull_request",
                "arguments": {
                    "repository_full_name": "o/r",
                    "base_branch": "main",
                    "head_branch": "agent/fix-association"
                }
            }),
            "o",
            "r"
        ));
        assert!(!requests_pull_request_creation(
            "mcpToolCall",
            &serde_json::json!({
                "type": "mcpToolCall",
                "server": "codex_apps",
                "tool": "github.create_pull_request",
                "arguments": {
                    "repository_full_name": "other/project",
                    "base_branch": "main",
                    "head_branch": "agent/fix-association"
                }
            }),
            "o",
            "r"
        ));
        assert!(requests_pull_request_creation(
            "mcpToolCall",
            &serde_json::json!({
                "server": "codex_apps",
                "tool": "github.create_pull_request",
                "arguments": "{\"repository_full_name\":\"o/r\",\"head_branch\":\"fix/string\"}"
            }),
            "o",
            "r"
        ));
        assert!(!requests_pull_request_creation(
            "mcpToolCall",
            &serde_json::json!({
                "type": "mcpToolCall",
                "server": "codex_apps",
                "tool": "github.create_pull_request",
                "repository": "other/project",
                "head_branch": "agent/fix-association",
                "arguments": "{undecodable"
            }),
            "o",
            "r"
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "mcpToolCall",
                &serde_json::json!({
                    "server": "codex_apps",
                    "tool": "github.create_pull_request",
                    "repository": "other/project",
                    "arguments": "{undecodable"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({
                    "operation": "create_pull_request",
                    "repository_full_name": "o/r"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Confirmed
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({
                    "action": "create_pr",
                    "repository_full_name": "o/r"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Confirmed
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({
                    "action": "create_pr",
                    "owner": "o",
                    "repository": "r"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Confirmed
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({
                    "action": "create_pr",
                    "owner": "o",
                    "repository": "r/"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Confirmed
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({
                    "action": "create_pr",
                    "owner": "other",
                    "repository": "r"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({
                    "action": "create_pr",
                    "repository": "r"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "connector",
                &serde_json::json!({
                    "operation": "create_pull_request",
                    "repository_full_name": "o/r"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({ "operation": "create_pull_request" }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "github",
                &serde_json::json!({
                    "operation": "create_pull_request",
                    "repository_full_name": "other/project"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(requests_pull_request_creation(
            "functions.exec",
            &serde_json::json!({"cmd": "gh pr create --head fix/other"}),
            "o",
            "r"
        ));
        assert!(requests_pull_request_creation(
            "github_rest",
            &serde_json::json!({
                "request": {
                    "method": "POST",
                    "url": "https://api.github.com/repos/o/r/pulls"
                }
            }),
            "o",
            "r"
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "http_request",
                &serde_json::json!({
                    "method": "POST",
                    "url": "https://api.github.com/repos/o/r/pulls"
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Confirmed
        ));
        assert!(could_request_pull_request_creation(
            "http_request",
            &serde_json::json!({
                "method": "POST",
                "url": "https://api.github.com/repos/o/r/pulls"
            })
        ));
        assert!(could_request_pull_request_creation(
            "fetch",
            &serde_json::json!({
                "request": {
                    "method": "POST",
                    "url": "https://api.github.com/repos/o/r/pulls"
                }
            })
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "mcpToolCall",
                &serde_json::json!({
                    "server": "codex_apps",
                    "repository_full_name": "o/r",
                    "tool": "api.graphql",
                    "arguments": {
                        "query": "mutation { createPullRequest(input: $input) { pullRequest { url } } }"
                    }
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Confirmed
        ));
        assert!(could_request_pull_request_creation(
            "mcpToolCall",
            &serde_json::json!({
                "server": "codex_apps",
                "repository_full_name": "o/r",
                "tool": "api.graphql",
                "arguments": {
                    "query": "mutation { createPullRequest(input: $input) { pullRequest { url } } }"
                }
            })
        ));
        let graphql_create = serde_json::json!({
            "query": "mutation { createPullRequest(input: $input) { pullRequest { url } } }"
        });
        assert!(matches!(
            classify_pull_request_creation("graphql", &graphql_create, "o", "r"),
            PullRequestCreationRequest::Confirmed
        ));
        assert!(could_request_pull_request_creation(
            "graphql",
            &graphql_create
        ));
        assert!(could_request_pull_request_creation(
            "run_query",
            &graphql_create
        ));
        let click_create = serde_json::json!({"text": "Create pull request"});
        assert!(!matches!(
            classify_pull_request_creation("browser_click", &click_create, "o", "r"),
            PullRequestCreationRequest::Rejected
        ));
        assert!(could_request_pull_request_creation(
            "browser_click",
            &click_create
        ));
        assert!(!could_request_pull_request_creation(
            "browser_click",
            &serde_json::json!({"text": "Create issue"})
        ));
        assert!(!could_request_pull_request_creation(
            "github_api",
            &serde_json::json!({"method": "GET", "url": "/repos/o/r/pulls"})
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "mcpToolCall",
                &serde_json::json!({
                    "server": "codex_apps",
                    "repository_full_name": "other/project",
                    "tool": "github.create_pull_request",
                    "arguments": { "repository_full_name": "o/r" }
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(matches!(
            classify_pull_request_creation(
                "mcpToolCall",
                &serde_json::json!({
                    "server": "untrusted",
                    "tool": "github.create_pull_request",
                    "arguments": { "repository_full_name": "o/r" }
                }),
                "o",
                "r"
            ),
            PullRequestCreationRequest::Rejected
        ));
        assert!(requests_pull_request_creation(
            "functions.exec",
            &serde_json::json!({
                "cmd": "gh api repos/o/r/pulls --method POST --field title=test"
            }),
            "o",
            "r"
        ));
        assert!(could_request_pull_request_creation(
            "functions.exec",
            &serde_json::json!({
                "cmd": "gh api -X POST /repos/o/r/pulls --field title=test"
            })
        ));
        for command in [
            "gh api /repos/o/r/pulls --field=title=test",
            "gh api /repos/o/r/pulls -Ftitle=test",
            "gh api /repos/o/r/pulls --input payload.json",
            "gh api -X \"POST\" /repos/o/r/pulls",
            "curl /repos/o/r/pulls --data payload.json",
            "curl /repos/o/r/pulls --json payload.json",
        ] {
            let args = serde_json::json!({ "cmd": command });
            assert!(could_request_pull_request_creation("functions.exec", &args));
            assert!(requests_pull_request_creation(
                "functions.exec",
                &args,
                "o",
                "r"
            ));
        }
        for command in [
            "env -i gh api /repos/o/r/pulls -f title=test",
            "sudo -u root gh api /repos/o/r/pulls -f title=test",
            "sudo A=B gh api /repos/o/r/pulls -f title=test",
            "command -p gh api /repos/o/r/pulls -f title=test",
            "nohup gh api /repos/o/r/pulls -f title=test",
            "exec -c gh api /repos/o/r/pulls -f title=test",
            "timeout --signal TERM 30 gh api /repos/o/r/pulls -f title=test",
            "nice -n 5 gh api /repos/o/r/pulls -f title=test",
            "stdbuf -o L gh api /repos/o/r/pulls -f title=test",
        ] {
            let args = serde_json::json!({ "cmd": command });
            assert!(could_request_pull_request_creation("functions.exec", &args));
            assert!(requests_pull_request_creation(
                "functions.exec",
                &args,
                "o",
                "r"
            ));
        }
        let explicit_get = serde_json::json!({
            "cmd": "gh api --method GET /repos/o/r/pulls -f state=open"
        });
        assert!(!could_request_pull_request_creation(
            "functions.exec",
            &explicit_get
        ));
        assert!(!requests_pull_request_creation(
            "functions.exec",
            &explicit_get,
            "o",
            "r"
        ));
        let independent_invocations = serde_json::json!({
            "cmd": concat!(
                "gh api -X POST /repos/o/r/pulls --field title=test && ",
                "gh api -X GET /repos/o/r/pulls"
            )
        });
        assert!(could_request_pull_request_creation(
            "functions.exec",
            &independent_invocations
        ));
        assert!(requests_pull_request_creation(
            "functions.exec",
            &independent_invocations,
            "o",
            "r"
        ));
        let curl_read = serde_json::json!({
            "cmd": "curl -fsSL https://api.github.com/repos/o/r/pulls"
        });
        assert!(!could_request_pull_request_creation(
            "functions.exec",
            &curl_read
        ));
        assert!(!requests_pull_request_creation(
            "functions.exec",
            &curl_read,
            "o",
            "r"
        ));
        for (tool, args) in [
            (
                "terminal",
                serde_json::json!({
                    "input": "gh api /repos/o/r/pulls --field title=test"
                }),
            ),
            (
                "functions.exec",
                serde_json::json!({
                    "cmd": "bash -c 'gh api /repos/o/r/pulls -f title=test'"
                }),
            ),
            (
                "functions.exec",
                serde_json::json!({
                    "cmd": "(gh api /repos/o/r/pulls -f title=test)"
                }),
            ),
        ] {
            assert!(could_request_pull_request_creation(tool, &args));
            assert!(requests_pull_request_creation(tool, &args, "o", "r"));
        }
        for args in [
            serde_json::json!({
                "cmd": "echo safe",
                "description": "gh api /repos/o/r/pulls -f title=forged"
            }),
            serde_json::json!({
                "cmd": "echo bash -c 'gh api /repos/o/r/pulls -f title=forged'"
            }),
            serde_json::json!({
                "cmd": "bash --rcfile 'gh api /repos/o/r/pulls -f title=forged'"
            }),
            serde_json::json!({
                "cmd": "command -v gh api /repos/o/r/pulls -f title=forged"
            }),
            serde_json::json!({
                "cmd": "timeout 30 A=B gh api /repos/o/r/pulls -f title=forged"
            }),
            serde_json::json!({
                "cmd": "nice 50 gh api /repos/o/r/pulls -f title=forged"
            }),
            serde_json::json!({
                "cmd": "nice é gh api /repos/o/r/pulls -f title=forged"
            }),
        ] {
            assert!(!could_request_pull_request_creation(
                "functions.exec",
                &args
            ));
            assert!(!requests_pull_request_creation(
                "functions.exec",
                &args,
                "o",
                "r"
            ));
        }
        assert!(!requests_pull_request_creation(
            "mcpToolCall",
            &serde_json::json!({
                "server": "codex_apps",
                "tool": "github.get_pull_request",
                "arguments": { "repository_full_name": "o/r", "pr_number": 75 }
            }),
            "o",
            "r"
        ));
        assert!(!requests_pull_request_creation(
            "functions.exec",
            &serde_json::json!({"cmd": "gh pr list --json url"}),
            "o",
            "r"
        ));
        assert!(!requests_pull_request_creation(
            "github_rest",
            &serde_json::json!({
                "request": {
                    "method": "POST",
                    "url": "https://api.github.com/repos/o/r/pulls/75/comments"
                }
            }),
            "o",
            "r"
        ));
        assert!(requests_remote_ref_mutation(
            "functions.exec",
            &serde_json::json!({"cmd": "git push origin HEAD:fix/manual-pr"}),
            "o",
            "r"
        ));
        assert!(requests_remote_ref_mutation(
            "functions.exec",
            &serde_json::json!({
                "cmd": "gh api repos/o/r/git/refs --method POST --field ref=refs/heads/fix/api"
            }),
            "o",
            "r"
        ));
        assert!(!requests_remote_ref_mutation(
            "functions.exec",
            &serde_json::json!({"cmd": "git fetch origin fix/unrelated"}),
            "o",
            "r"
        ));
    }

    #[test]
    fn records_pull_request_created_through_generic_mcp_wrapper() {
        let events = [
            Event::ToolRequested {
                turn: 1,
                call_id: "create-pr".into(),
                tool: "mcpToolCall".into(),
                args: serde_json::json!({
                    "type": "mcpToolCall",
                    "server": "codex_apps",
                    "tool": "github.create_pull_request",
                    "arguments": {
                        "repository_full_name": "jimsimon/trouve",
                        "base_branch": "main",
                        "head_branch": "agent/separate-harness-search-readmes",
                        "title": "Follow-up to https://github.com/jimsimon/trouve/pull/274"
                    }
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "create-pr".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "result": {
                        "structuredContent": {
                            "number": 267,
                            "url": "https://github.com/jimsimon/trouve/pull/267"
                        }
                    }
                }),
                execution_duration_ms: Some(1_492),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "create-other-pr".into(),
                tool: "mcpToolCall".into(),
                args: serde_json::json!({
                    "type": "mcpToolCall",
                    "server": "codex_apps",
                    "tool": "github.create_pull_request",
                    "arguments": {
                        "repository_full_name": "other/project",
                        "base_branch": "main",
                        "head_branch": "agent/separate-harness-search-readmes"
                    }
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "create-other-pr".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "result": {
                        "structuredContent": {
                            "number": 42,
                            "url": "https://github.com/other/project/pull/42"
                        }
                    }
                }),
                execution_duration_ms: Some(800),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "create-malformed-pr".into(),
                tool: "mcpToolCall".into(),
                args: serde_json::json!({
                    "type": "mcpToolCall",
                    "server": "codex_apps",
                    "tool": "github.create_pull_request",
                    "repository": "jimsimon/trouve",
                    "head_branch": "agent/separate-harness-search-readmes",
                    "arguments": "{undecodable"
                }),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "create-malformed-pr".into(),
                chunk: "Opened https://github.com/jimsimon/trouve/pull/268".into(),
            },
            Event::ToolCompleted {
                call_id: "create-malformed-pr".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "result": {
                        "structuredContent": {
                            "number": 268
                        }
                    }
                }),
                execution_duration_ms: Some(600),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "create-malformed-other-pr".into(),
                tool: "mcpToolCall".into(),
                args: serde_json::json!({
                    "server": "codex_apps",
                    "tool": "github.create_pull_request",
                    "repository": "other/project",
                    "arguments": "{undecodable"
                }),
                requires_approval: false,
            },
            Event::ToolOutput {
                call_id: "create-malformed-other-pr".into(),
                chunk: "Opened https://github.com/jimsimon/trouve/pull/273".into(),
            },
            Event::ToolCompleted {
                call_id: "create-malformed-other-pr".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "url": "https://github.com/jimsimon/trouve/pull/273"
                }),
                execution_duration_ms: Some(400),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "read-pr".into(),
                tool: "mcpToolCall".into(),
                args: serde_json::json!({
                    "server": "codex_apps",
                    "tool": "github.get_pull_request",
                    "arguments": {
                        "repository_full_name": "jimsimon/trouve",
                        "pr_number": 269
                    }
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "read-pr".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "url": "https://github.com/jimsimon/trouve/pull/269"
                }),
                execution_duration_ms: Some(300),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "create-other-with-local-result".into(),
                tool: "mcpToolCall".into(),
                args: serde_json::json!({
                    "server": "codex_apps",
                    "tool": "github.create_pull_request",
                    "arguments": {
                        "repository_full_name": "other/project",
                        "head_branch": "agent/separate-harness-search-readmes"
                    }
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "create-other-with-local-result".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "url": "https://github.com/jimsimon/trouve/pull/270"
                }),
                execution_duration_ms: Some(400),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "untrusted-create".into(),
                tool: "mcpToolCall".into(),
                args: serde_json::json!({
                    "server": "untrusted",
                    "tool": "github.create_pull_request",
                    "arguments": {
                        "repository_full_name": "jimsimon/trouve",
                        "head_branch": "agent/separate-harness-search-readmes"
                    }
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "untrusted-create".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "url": "https://github.com/jimsimon/trouve/pull/275"
                }),
                execution_duration_ms: Some(400),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "create-structured-pr".into(),
                tool: "github".into(),
                args: serde_json::json!({
                    "operation": "create_pull_request",
                    "repository_full_name": "jimsimon/trouve",
                    "head_branch": "agent/separate-harness-search-readmes"
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "create-structured-pr".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "url": "https://github.com/jimsimon/trouve/pull/271"
                }),
                execution_duration_ms: Some(400),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "forge-structured-pr".into(),
                tool: "connector".into(),
                args: serde_json::json!({
                    "action": "create_pr",
                    "repository_full_name": "jimsimon/trouve"
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "forge-structured-pr".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({
                    "url": "https://github.com/jimsimon/trouve/pull/272"
                }),
                execution_duration_ms: Some(400),
            },
        ];

        let evidence = pr_evidence_from_events(events, "github.com", "jimsimon", "trouve");
        assert_eq!(evidence.numbers, HashSet::from([267, 268, 271]));
        assert_eq!(evidence.successful_tool_args.len(), 3);
    }

    #[test]
    fn workspace_list_items_cache_normalized_remote_identity() {
        let first_directory = tempfile::tempdir().unwrap();
        let second_directory = tempfile::tempdir().unwrap();
        for repository in [first_directory.path(), second_directory.path()] {
            let mut init = std::process::Command::new("git");
            init.args(["init", "-b", "main"]).arg(repository);
            assert!(trouve_process::output(&mut init).unwrap().status.success());
            let mut remote = std::process::Command::new("git");
            remote.arg("-C").arg(repository).args([
                "remote",
                "add",
                "origin",
                "git@GitHub.com:Acme/Widgets.git",
            ]);
            assert!(
                trouve_process::output(&mut remote)
                    .unwrap()
                    .status
                    .success()
            );
        }

        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        );
        let first = engine
            .register_workspace(
                first_directory.path().to_str().unwrap(),
                Some("first clone".into()),
            )
            .unwrap();
        let second = engine
            .register_workspace(
                second_directory.path().to_str().unwrap(),
                Some("second clone".into()),
            )
            .unwrap();

        assert_eq!(first.repository_key, second.repository_key);
        assert_eq!(first.repository_name.as_deref(), Some("Widgets"));
        assert_eq!(second.repository_name.as_deref(), Some("Widgets"));

        let mut remote = std::process::Command::new("git");
        remote.arg("-C").arg(second_directory.path()).args([
            "remote",
            "set-url",
            "origin",
            "git@github.com:Acme/Other.git",
        ]);
        assert!(
            trouve_process::output(&mut remote)
                .unwrap()
                .status
                .success()
        );
        engine
            .workspace_list_cache
            .lock()
            .unwrap()
            .get_mut(&second.id)
            .unwrap()
            .refreshed_at = Instant::now() - WORKSPACE_LIST_CACHE_TTL;

        let listed = engine.list_workspaces().unwrap();
        let refreshed = listed
            .iter()
            .find(|workspace| workspace.id == second.id)
            .unwrap();
        assert_ne!(refreshed.repository_key, first.repository_key);
        assert_eq!(refreshed.repository_name.as_deref(), Some("Other"));

        drop(first_directory);
        drop(second_directory);
        let cached = engine.list_workspaces().unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(
            cached
                .iter()
                .find(|workspace| workspace.id == second.id)
                .and_then(|workspace| workspace.repository_name.as_deref()),
            Some("Other")
        );
    }

    #[test]
    fn concurrent_first_workspace_registrations_share_one_workspace() {
        let repository = tempfile::tempdir().unwrap();
        let mut init = std::process::Command::new("git");
        init.args(["init", "-b", "main"]).arg(repository.path());
        assert!(trouve_process::output(&mut init).unwrap().status.success());

        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let repository_path = repository.path().to_path_buf();
        let (first_prepared_tx, first_prepared_rx) = std::sync::mpsc::channel();
        let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
        let first_engine = Arc::clone(&engine);
        let first_path = repository_path.clone();
        let first = std::thread::spawn(move || {
            first_engine.register_workspace_with(
                first_path.to_str().unwrap(),
                Some("first registration".into()),
                |contended| assert!(!contended),
                || {
                    first_prepared_tx.send(()).unwrap();
                    release_first_rx.recv().unwrap();
                },
            )
        });
        first_prepared_rx.recv().unwrap();

        let (second_lock_tx, second_lock_rx) = std::sync::mpsc::channel();
        let second_engine = Arc::clone(&engine);
        let second = std::thread::spawn(move || {
            second_engine.register_workspace_with(
                repository_path.to_str().unwrap(),
                Some("second registration".into()),
                |contended| second_lock_tx.send(contended).unwrap(),
                || {},
            )
        });
        assert!(second_lock_rx.recv().unwrap());
        release_first_tx.send(()).unwrap();

        let registrations = [
            first.join().unwrap().unwrap(),
            second.join().unwrap().unwrap(),
        ];

        assert_eq!(registrations[0].id, registrations[1].id);
        assert_eq!(engine.list_workspaces().unwrap().len(), 1);
        assert_eq!(
            engine
                .store
                .events_after(&Scope::Server, 0)
                .unwrap()
                .into_iter()
                .filter(|envelope| matches!(envelope.event, Event::WorkspaceRegistered { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn workspace_close_waits_for_reregistration_and_remains_final() {
        let repository = tempfile::tempdir().unwrap();
        let mut init = std::process::Command::new("git");
        init.args(["init", "-b", "main"]).arg(repository.path());
        assert!(trouve_process::output(&mut init).unwrap().status.success());

        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let workspace = engine
            .register_workspace(repository.path().to_str().unwrap(), None)
            .unwrap();
        let workspace_id = workspace.id.clone();
        let repository_path = repository.path().to_path_buf();
        let (prepared_tx, prepared_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let registration_engine = Arc::clone(&engine);
        let registration = std::thread::spawn(move || {
            registration_engine.register_workspace_with(
                repository_path.to_str().unwrap(),
                None,
                |_| {},
                || {
                    prepared_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            )
        });
        prepared_rx.recv().unwrap();

        let (close_lock_tx, close_lock_rx) = std::sync::mpsc::channel();
        let close_engine = Arc::clone(&engine);
        let close_workspace_id = workspace_id.clone();
        let close = std::thread::spawn(move || {
            close_engine.close_workspace_with(&close_workspace_id, |contended| {
                close_lock_tx.send(contended).unwrap();
            })
        });
        assert!(close_lock_rx.recv().unwrap());
        release_tx.send(()).unwrap();
        registration.join().unwrap().unwrap();
        close.join().unwrap().unwrap();

        assert!(engine.list_workspaces().unwrap().is_empty());
        let lifecycle = engine
            .store
            .events_after(&Scope::Server, 0)
            .unwrap()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::WorkspaceRegistered {
                    workspace_id: registered_id,
                    ..
                } if registered_id == workspace_id => Some("registered"),
                Event::WorkspaceClosed {
                    workspace_id: closed_id,
                } if closed_id == workspace_id => Some("closed"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(lifecycle, ["registered", "closed"]);
    }

    #[test]
    fn workspace_list_identity_refresh_is_single_flight_per_workspace() {
        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let workspace = Workspace {
            id: "ws_single_flight".into(),
            name: "single flight".into(),
            path: data.path().to_string_lossy().into_owned(),
        };
        engine.workspace_list_cache.lock().unwrap().insert(
            workspace.id.clone(),
            WorkspaceListCacheEntry {
                item: WorkspaceListItem {
                    id: workspace.id.clone(),
                    name: workspace.name.clone(),
                    path: workspace.path.clone(),
                    repository_key: Some("remote:github.com/acme/widgets".into()),
                    repository_name: Some("widgets".into()),
                },
                refreshed_at: Instant::now() - WORKSPACE_LIST_CACHE_TTL,
            },
        );
        let start = Arc::new(std::sync::Barrier::new(3));
        let resolutions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles = (0..2)
            .map(|_| {
                let engine = Arc::clone(&engine);
                let workspace = workspace.clone();
                let start = Arc::clone(&start);
                let resolutions = Arc::clone(&resolutions);
                std::thread::spawn(move || {
                    start.wait();
                    engine.cached_workspace_list_item_with(
                        workspace,
                        Instant::now() + Duration::from_secs(1),
                        move |workspace, _timeout| {
                            resolutions.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(50));
                            WorkspaceListItem {
                                id: workspace.id,
                                name: workspace.name,
                                path: workspace.path,
                                repository_key: Some("remote:github.com/acme/widgets".into()),
                                repository_name: Some("widgets".into()),
                            }
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        let items = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(resolutions.load(Ordering::SeqCst), 1);
        assert!(items.iter().all(|item| {
            item.repository_key.as_deref() == Some("remote:github.com/acme/widgets")
        }));
    }

    #[test]
    fn workspace_list_identity_refreshes_share_one_request_deadline() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        );
        let first = Workspace {
            id: "ws_budget_first".into(),
            name: "first".into(),
            path: data.path().join("first").to_string_lossy().into_owned(),
        };
        let second = Workspace {
            id: "ws_budget_second".into(),
            name: "second".into(),
            path: data.path().join("second").to_string_lossy().into_owned(),
        };
        let resolutions = std::sync::atomic::AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_millis(30);
        let first_item =
            engine.cached_workspace_list_item_with(first, deadline, |workspace, remaining| {
                resolutions.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(remaining + Duration::from_millis(10));
                Engine::fallback_workspace_list_item(workspace)
            });
        let second_item =
            engine.cached_workspace_list_item_with(second, deadline, |workspace, _remaining| {
                resolutions.fetch_add(1, Ordering::SeqCst);
                Engine::fallback_workspace_list_item(workspace)
            });

        assert_eq!(resolutions.load(Ordering::SeqCst), 1);
        assert_eq!(
            first_item.repository_key.as_deref(),
            Some("workspace:ws_budget_first")
        );
        assert_eq!(
            second_item.repository_key.as_deref(),
            Some("workspace:ws_budget_second")
        );
    }

    #[test]
    fn cancelled_review_workspace_registration_does_not_commit() {
        let repository = tempfile::tempdir().unwrap();
        let mut init = std::process::Command::new("git");
        init.args(["init", "-b", "main"]).arg(repository.path());
        assert!(trouve_process::output(&mut init).unwrap().status.success());

        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let commit_fence = ReviewWorkspaceRegistrationFence::default();
        let error = engine
            .register_review_workspace_with(
                repository.path().to_str().unwrap(),
                Some("cancelled review".into()),
                &cancel,
                &commit_fence,
                || cancel.cancel(),
                || {},
            )
            .unwrap_err();

        assert!(error.to_string().starts_with("stale:"));
        assert!(engine.list_workspaces().unwrap().is_empty());
    }

    #[test]
    fn cancellation_compensates_registration_admitted_before_timeout() {
        let repository = tempfile::tempdir().unwrap();
        let mut init = std::process::Command::new("git");
        init.args(["init", "-b", "main"]).arg(repository.path());
        assert!(trouve_process::output(&mut init).unwrap().status.success());

        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        let commit_fence = Arc::new(ReviewWorkspaceRegistrationFence::default());
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let registration_engine = Arc::clone(&engine);
        let registration_cancel = cancel.clone();
        let registration_fence = Arc::clone(&commit_fence);
        let repository_path = repository.path().to_path_buf();
        let registration = std::thread::spawn(move || {
            registration_engine.register_review_workspace_with(
                repository_path.to_str().unwrap(),
                Some("timed out review".into()),
                &registration_cancel,
                &registration_fence,
                || {},
                || {
                    admitted_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            )
        });
        admitted_rx.recv().unwrap();

        let cancellation_engine = Arc::clone(&engine);
        let cancellation_token = cancel.clone();
        let cancellation_fence = Arc::clone(&commit_fence);
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let cancellation = std::thread::spawn(move || {
            let result = cancellation_engine
                .cancel_review_workspace_registration(&cancellation_token, &cancellation_fence);
            cancelled_tx.send(()).unwrap();
            result
        });
        assert!(matches!(
            cancelled_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        release_tx.send(()).unwrap();

        let workspace = registration.join().unwrap().unwrap();
        cancellation.join().unwrap().unwrap();

        assert!(cancel.is_cancelled());
        assert!(engine.list_workspaces().unwrap().is_empty());
        let lifecycle = engine
            .store
            .events_after(&Scope::Server, 0)
            .unwrap()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::WorkspaceRegistered { workspace_id, .. } if workspace_id == workspace.id => {
                    Some("registered")
                }
                Event::WorkspaceClosed { workspace_id } if workspace_id == workspace.id => {
                    Some("closed")
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(lifecycle, ["registered", "closed"]);
    }

    #[test]
    fn cancellation_recloses_workspace_reopened_before_timeout() {
        let repository = tempfile::tempdir().unwrap();
        let mut init = std::process::Command::new("git");
        init.args(["init", "-b", "main"]).arg(repository.path());
        assert!(trouve_process::output(&mut init).unwrap().status.success());

        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let workspace = engine
            .register_workspace(repository.path().to_str().unwrap(), None)
            .unwrap();
        engine.close_workspace(&workspace.id).unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let commit_fence = Arc::new(ReviewWorkspaceRegistrationFence::default());
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let registration_engine = Arc::clone(&engine);
        let registration_cancel = cancel.clone();
        let registration_fence = Arc::clone(&commit_fence);
        let repository_path = repository.path().to_path_buf();
        let registration = std::thread::spawn(move || {
            registration_engine.register_review_workspace_with(
                repository_path.to_str().unwrap(),
                None,
                &registration_cancel,
                &registration_fence,
                || {},
                || {
                    admitted_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            )
        });
        admitted_rx.recv().unwrap();

        let cancellation_engine = Arc::clone(&engine);
        let cancellation_token = cancel.clone();
        let cancellation_fence = Arc::clone(&commit_fence);
        let cancellation = std::thread::spawn(move || {
            cancellation_engine
                .cancel_review_workspace_registration(&cancellation_token, &cancellation_fence)
        });
        release_tx.send(()).unwrap();

        let reopened = registration.join().unwrap().unwrap();
        cancellation.join().unwrap().unwrap();

        assert_eq!(reopened.id, workspace.id);
        assert!(engine.list_workspaces().unwrap().is_empty());
        let lifecycle = engine
            .store
            .events_after(&Scope::Server, 0)
            .unwrap()
            .into_iter()
            .filter_map(|envelope| match envelope.event {
                Event::WorkspaceRegistered { workspace_id, .. } if workspace_id == workspace.id => {
                    Some("registered")
                }
                Event::WorkspaceClosed { workspace_id } if workspace_id == workspace.id => {
                    Some("closed")
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(lifecycle, ["registered", "closed", "registered", "closed"]);
    }

    #[test]
    fn archive_and_workspace_close_tear_down_terminals_until_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new("git");
        command.args(["init", "-b", "main"]).arg(dir.path());
        assert!(
            trouve_process::output(&mut command)
                .unwrap()
                .status
                .success()
        );
        let store = Store::open_in_memory().unwrap();
        let engine = Engine::new(store, dir.path().to_path_buf(), &Config::default());
        let workspace = engine
            .register_workspace(
                dir.path().to_str().unwrap(),
                Some("terminal archive".into()),
            )
            .unwrap();
        let workspace_id = workspace.id.clone();
        let session = Session {
            id: "se_terminal_archive".into(),
            workspace_id: workspace_id.clone(),
            title: "terminal archive".into(),
            branch: "trouve/terminal-archive".into(),
            worktree_path: workspace.path,
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        engine.store.insert_session(&session).unwrap();

        let generated = engine
            .update_session(
                &session.id,
                &UpdateSessionRequest {
                    title: Some("Generated title".into()),
                    expected_title: Some("terminal archive".into()),
                    archived: None,
                },
            )
            .unwrap();
        assert_eq!(generated.title, "Generated title");
        assert!(matches!(
            engine.update_session(
                &session.id,
                &UpdateSessionRequest {
                    title: Some("Stale generated title".into()),
                    expected_title: Some("terminal archive".into()),
                    archived: None,
                },
            ),
            Err(EngineError::Conflict(_))
        ));
        assert_eq!(
            engine.get_session(&session.id).unwrap().title,
            "Generated title"
        );

        engine.open_terminal(&session.id, 80, 24).unwrap();
        engine.create_terminal(&session.id, 80, 24).unwrap();
        assert_eq!(engine.list_terminals(&session.id).unwrap().len(), 2);

        let archived = engine
            .update_session(
                &session.id,
                &UpdateSessionRequest {
                    title: None,
                    archived: Some(true),
                    expected_title: None,
                },
            )
            .unwrap();
        assert!(archived.archived);
        assert!(engine.list_terminals(&session.id).unwrap().is_empty());
        assert!(matches!(
            engine.create_terminal(&session.id, 80, 24),
            Err(EngineError::Conflict(_))
        ));

        let unarchived = engine
            .update_session(
                &session.id,
                &UpdateSessionRequest {
                    title: None,
                    archived: Some(false),
                    expected_title: None,
                },
            )
            .unwrap();
        assert!(!unarchived.archived);
        assert!(engine.create_terminal(&session.id, 80, 24).is_ok());

        engine.close_workspace(&workspace_id).unwrap();
        assert!(engine.list_terminals(&session.id).unwrap().is_empty());
        assert!(matches!(
            engine.create_terminal(&session.id, 80, 24),
            Err(EngineError::Conflict(_))
        ));

        let reopened = engine
            .register_workspace(dir.path().to_str().unwrap(), None)
            .unwrap();
        assert_eq!(reopened.id, workspace_id);
        assert!(engine.create_terminal(&session.id, 80, 24).is_ok());
    }

    #[test]
    fn loopback_base_url_requires_an_exact_loopback_host() {
        for url in [
            "http://localhost:11434",
            "http://LOCALHOST:11434/v1",
            "http://127.0.0.1:8080/v1",
            "https://127.1.2.3", // whole 127/8 block is loopback
            "http://[::1]:8000",
        ] {
            assert!(is_loopback_base_url(url), "should be loopback: {url}");
        }
        for url in [
            // Suffix tricks: remote hosts that merely contain a loopback
            // string must not be treated as local, or offline mode would
            // enable prompts that still need the internet.
            "https://localhost.attacker.example",
            "https://127.0.0.1.attacker.example",
            "https://attacker.example/path?q=://localhost",
            "https://user:pw@attacker.example#://127.0.0.1",
            "https://api.example.com",
            "http://192.168.1.10:11434",
            "http://[::2]:8000",
            "not a url",
        ] {
            assert!(!is_loopback_base_url(url), "should not be loopback: {url}");
        }
    }

    #[test]
    fn cli_command_prefers_explicit_then_managed_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let managed =
            trouve_agents::install::managed_bin(tmp.path(), trouve_agents::install::CliId::Codex);
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::write(&managed, b"stub").unwrap();

        assert_eq!(
            resolved_cli_command("codex-app-server", None, tmp.path()),
            Some(managed.to_string_lossy().into_owned())
        );
        assert_eq!(
            resolved_cli_command(
                "codex-app-server",
                Some("/opt/custom/codex".into()),
                tmp.path()
            )
            .as_deref(),
            Some("/opt/custom/codex")
        );
        assert_eq!(
            resolved_cli_command("openai-compat", None, tmp.path()),
            None
        );
    }

    #[tokio::test]
    async fn todo_tool_persists_and_emits_thread_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_todo".into(),
            name: "todo".into(),
            path: tmp.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_todo".into(),
            workspace_id: workspace.id.clone(),
            title: "todo".into(),
            branch: "main".into(),
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_todo".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "test/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let config = Config {
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Arc::new(Engine::new(store.clone(), tmp.path().into(), &config));
        let ctx = ToolCtx {
            worktree: tmp.path().into(),
            thread_id: thread.id.clone(),
            ..Default::default()
        };
        let call = trouve_providers::ToolCallRequest {
            id: "call_todo".into(),
            name: "todo_write".into(),
            arguments: serde_json::json!({"todos": [
                {"id": "one", "content": "First", "status": "in_progress"}
            ]}),
        };

        engine
            .handle_tool_call(
                &session,
                &thread,
                1,
                &personas::fallback_persona(),
                &ctx,
                &call,
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();

        let stored = store.thread(&thread.id).unwrap().unwrap();
        assert_eq!(stored.todos.len(), 1);
        assert_eq!(
            stored.todos[0].status,
            trouve_protocol::TodoStatus::InProgress
        );
        let events = store
            .events_after(&Scope::Thread(thread.id.clone()), 0)
            .unwrap();
        assert!(events.iter().any(|env| matches!(
            &env.event,
            Event::TodosUpdated { todos }
                if todos.len() == 1 && todos[0].id == "one"
        )));

        // Vendor-native TodoWrite completions can be an acknowledgement
        // rather than the updated list. Fall back to the paired start args,
        // preserving existing items when the vendor requests a merge.
        let vendor_result = serde_json::json!("Todos updated");
        let vendor_args = serde_json::json!({"merge": true, "todos": [
            {"content": "Second", "activeForm": "Working on second", "status": "pending"}
        ]});
        let vendor_todos = engine
            .persist_todos_from_result(
                &thread.id,
                "TodoWrite",
                ToolStatus::Ok,
                &vendor_result,
                Some(&vendor_args),
            )
            .unwrap()
            .unwrap();
        assert_eq!(vendor_todos.len(), 2);
        assert_eq!(
            vendor_todos[0].status,
            trouve_protocol::TodoStatus::InProgress
        );
        assert_eq!(vendor_todos[1].id, "vendor:Second");
        assert_eq!(
            store.thread(&thread.id).unwrap().unwrap().todos,
            vendor_todos
        );

        // An external MCP server may use the same basename, or a provider may
        // report a generic wrapper whose nested tool is named `todo_write`.
        // Neither is allowed to replace trouve's authoritative thread TODOs
        // or synthesize a TodosUpdated event.
        let todos_updated_before = store
            .events_after(&Scope::Thread(thread.id.clone()), 0)
            .unwrap()
            .iter()
            .filter(|env| matches!(env.event, Event::TodosUpdated { .. }))
            .count();
        let external_engine = Arc::new(
            Engine::new(store.clone(), tmp.path().into(), &config)
                .with_executor(Arc::new(SuccessfulTodoExecutor)),
        );
        let mut external_thread = thread.clone();
        external_thread.permission_mode = trouve_protocol::PermissionMode::Yolo;
        let external_ctx = ToolCtx {
            worktree: tmp.path().into(),
            thread_id: thread.id.clone(),
            ..Default::default()
        };
        let mode = personas::fallback_persona();
        for (turn, call) in [
            (
                2,
                trouve_providers::ToolCallRequest {
                    id: "call_external_todo".into(),
                    name: "mcp__external__todo_write".into(),
                    arguments: serde_json::json!({"todos": [
                        {"id": "external", "content": "External", "status": "completed"}
                    ]}),
                },
            ),
            (
                3,
                trouve_providers::ToolCallRequest {
                    id: "call_external_todo_wrapper".into(),
                    name: "mcpToolCall".into(),
                    arguments: serde_json::json!({
                        "server": "external",
                        "tool": "todo_write",
                        "arguments": {"todos": [
                            {"id": "external", "content": "External", "status": "completed"}
                        ]}
                    }),
                },
            ),
        ] {
            external_engine
                .handle_tool_call(
                    &session,
                    &external_thread,
                    turn,
                    &mode,
                    &external_ctx,
                    &call,
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            store.thread(&thread.id).unwrap().unwrap().todos,
            vendor_todos
        );
        let todos_updated_after = store
            .events_after(&Scope::Thread(thread.id.clone()), 0)
            .unwrap()
            .iter()
            .filter(|env| matches!(env.event, Event::TodosUpdated { .. }))
            .count();
        assert_eq!(todos_updated_after, todos_updated_before);

        // The reserved first-party bridge namespace is still authoritative if
        // a supported provider exposes the native tool under that identifier.
        let bridged = external_engine
            .persist_todos_from_result(
                &thread.id,
                "mcp__trouve__todo_write",
                ToolStatus::Ok,
                &serde_json::json!({"todos": [
                    {"id": "bridged", "content": "Bridged", "status": "pending"}
                ]}),
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(bridged[0].id, "bridged");
    }

    #[test]
    fn history_digest_renders_text_skips_tools_and_caps_length() {
        // Empty transcript: nothing to hand off.
        assert_eq!(render_history_digest(&[], false), None);
        assert_eq!(
            render_history_digest(&[Message::System("prompt".into())], false),
            None
        );

        let messages = [
            Message::System("mode prompt".into()),
            Message::User("add a login page".into()),
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![trouve_providers::ToolCallRequest {
                    id: "1".into(),
                    name: "write_file".into(),
                    arguments: "{}".into(),
                }],
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "1".into(),
                content: "long tool output that should not appear".into(),
                images: vec![],
            },
            Message::Assistant {
                content: "Done — login page added.".into(),
                tool_calls: vec![],
                reasoning: vec![],
            },
        ];
        let digest = render_history_digest(&messages, false).unwrap();
        assert!(digest.contains("User:\nadd a login page"));
        assert!(digest.contains("[ran tools: write_file]"));
        assert!(digest.contains("Done — login page added."));
        assert!(!digest.contains("should not appear"));
        assert!(!digest.contains("mode prompt"));
        assert!(digest.starts_with("[Handoff: you are continuing"));

        // Resumed sessions get catch-up framing instead.
        let digest = render_history_digest(&messages, true).unwrap();
        assert!(digest.starts_with("[Handoff: since your last turn"));

        // Oversized transcripts lose their middle, keep head and tail, and
        // never split a multi-byte character.
        let long = "é".repeat(HISTORY_DIGEST_MAX);
        let digest = render_history_digest(
            &[
                Message::User(format!("start-marker {long}")),
                Message::User("end-marker".into()),
            ],
            false,
        )
        .unwrap();
        assert!(digest.len() < HISTORY_DIGEST_MAX + 1_000);
        assert!(digest.contains("start-marker"));
        assert!(digest.contains("end-marker"));
        // The cut points at the recovery hatch for the elided middle.
        assert!(digest.contains("truncated — recover specifics with the search_transcript tool"));
    }

    #[test]
    fn cap_chars_truncates_on_char_boundaries() {
        assert_eq!(cap_chars("short", 100), "short");
        let capped = cap_chars(&"é".repeat(100), 21);
        assert!(capped.starts_with("éééééééééé"));
        assert!(capped.ends_with("[truncated]"));
    }

    #[test]
    fn annotate_edit_lines_resolves_snippet_positions() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "one\ntwo\nthree\nfour\n").unwrap();

        // Single edit: hint points at the snippet's first line.
        let mut args = serde_json::json!({
            "file_path": "a.rs",
            "old_string": "two\nthree",
            "new_string": "TWO",
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert_eq!(args["_line"], 2);

        // MultiEdit: each edit is placed against the file with earlier
        // edits already applied ("four" moves up when lines collapse).
        let mut args = serde_json::json!({
            "file_path": "a.rs",
            "edits": [
                {"old_string": "two\nthree", "new_string": "TWO"},
                {"old_string": "four", "new_string": "FOUR"},
            ],
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert_eq!(args["edits"][0]["_line"], 2);
        assert_eq!(args["edits"][1]["_line"], 3);

        // Ambiguous or missing snippets get no hint; absolute paths and
        // unreadable files are handled without touching the args.
        std::fs::write(tmp.path().join("b.rs"), "dup\ndup\n").unwrap();
        let mut args = serde_json::json!({
            "file_path": tmp.path().join("b.rs").to_str().unwrap(),
            "old_string": "dup",
            "new_string": "d",
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert!(args.get("_line").is_none());
        let mut args = serde_json::json!({
            "file_path": "missing.rs",
            "old_string": "x",
            "new_string": "y",
        });
        annotate_edit_lines(tmp.path(), &mut args);
        assert!(args.get("_line").is_none());

        // Write-style args (no snippets) are left alone entirely.
        let mut args = serde_json::json!({"path": "a.rs", "content": "all new"});
        let before = args.clone();
        annotate_edit_lines(tmp.path(), &mut args);
        assert_eq!(args, before);
    }

    #[test]
    fn sanitize_transcript_repairs_dangling_tool_calls() {
        use trouve_providers::{Message, ToolCallRequest};
        let call = |id: &str| ToolCallRequest {
            id: id.to_string(),
            name: "shell".into(),
            arguments: serde_json::json!({}),
        };

        // A crash left two tool calls with only one result, then the next
        // turn's user message.
        let messages = vec![
            Message::User("do it".into()),
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![call("a"), call("b")],
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "a".into(),
                content: "ok".into(),
                images: vec![],
            },
            Message::User("next".into()),
        ];
        let out = sanitize_transcript(messages);
        // The missing result for "b" is synthesized right after "a"'s.
        match &out[2] {
            Message::ToolResult { call_id, .. } => assert_eq!(call_id, "a"),
            other => panic!("expected result a, got {other:?}"),
        }
        match &out[3] {
            Message::ToolResult {
                call_id, content, ..
            } => {
                assert_eq!(call_id, "b");
                assert!(content.contains("interrupted"));
            }
            other => panic!("expected synthesized result b, got {other:?}"),
        }
        assert!(matches!(&out[4], Message::User(u) if u == "next"));

        // An empty assistant message is dropped entirely.
        let out = sanitize_transcript(vec![
            Message::User("hi".into()),
            Message::Assistant {
                content: "   ".into(),
                tool_calls: vec![],
                reasoning: vec![],
            },
        ]);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Message::User(_)));

        // A well-formed transcript is unchanged in length and pairing.
        let clean = vec![
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![call("x")],
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "x".into(),
                content: "done".into(),
                images: vec![],
            },
        ];
        assert_eq!(sanitize_transcript(clean).len(), 2);
    }

    #[test]
    fn tool_call_ids_are_non_empty_and_unique_before_execution() {
        let mut calls = vec![
            trouve_providers::ToolCallRequest {
                id: String::new(),
                name: "read_one".into(),
                arguments: serde_json::json!({}),
            },
            trouve_providers::ToolCallRequest {
                id: "provider-id".into(),
                name: "read_two".into(),
                arguments: serde_json::json!({}),
            },
            trouve_providers::ToolCallRequest {
                id: "provider-id".into(),
                name: "read_three".into(),
                arguments: serde_json::json!({}),
            },
        ];
        normalize_tool_call_ids(&mut calls);
        assert!(calls.iter().all(|call| !call.id.is_empty()));
        assert_eq!(calls[1].id, "provider-id");
        assert_ne!(calls[0].id, calls[1].id);
        assert_ne!(calls[1].id, calls[2].id);
    }

    #[test]
    fn inherited_thinking_level_resolves_through_model_schema() {
        let mut inherited = serde_json::Map::new();
        inherit_thinking_option(&mut inherited, Some("low"), Some("high"));
        assert_eq!(inherited["thinking_level"], "low");

        let mut explicit =
            serde_json::Map::from_iter([("reasoning_effort".into(), serde_json::json!("medium"))]);
        inherit_thinking_option(&mut explicit, Some("low"), Some("high"));
        assert_eq!(explicit.len(), 1, "an explicit thread option wins");
        assert_eq!(explicit["reasoning_effort"], "medium");

        let model = trouve_protocol::ModelInfo {
            id: "codex/gpt".into(),
            display_name: "GPT".into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "reasoning_effort": {
                        "type": "string",
                        "enum": ["low", "medium", "high"],
                        "default": "medium"
                    }
                }
            }),
        };
        let mut options =
            serde_json::Map::from_iter([("thinking_level".into(), serde_json::json!("high"))]);
        normalize_thinking_option(&mut options, Some(&model));
        assert_eq!(
            options.get("reasoning_effort"),
            Some(&serde_json::json!("high"))
        );
        assert_eq!(
            resolved_thinking_level(&options, Some(&model)).as_deref(),
            Some("high")
        );
        assert!(!options.contains_key("thinking_level"));

        // A global token the selected model does not offer falls back to
        // that model's advertised default.
        options.remove("reasoning_effort");
        options.insert("thinking_level".into(), serde_json::json!("xhigh"));
        normalize_thinking_option(&mut options, Some(&model));
        assert_eq!(
            options.get("reasoning_effort"),
            Some(&serde_json::json!("medium"))
        );
        assert_eq!(
            resolved_thinking_level(&serde_json::Map::new(), Some(&model)).as_deref(),
            Some("medium"),
            "omitting an option uses the model schema default"
        );

        let fixed_model = trouve_protocol::ModelInfo {
            id: "anthropic/claude-fixed".into(),
            display_name: "Claude fixed".into(),
            options_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "thinking_budget_tokens": {
                        "type": "integer",
                        "minimum": 1024,
                        "maximum": 32768
                    }
                }
            }),
            ..model.clone()
        };
        let mut explicit_budget = serde_json::Map::from_iter([(
            "thinking_budget_tokens".into(),
            serde_json::json!(8192),
        )]);
        inherit_thinking_option(&mut explicit_budget, Some("16384"), None);
        assert_eq!(explicit_budget.len(), 1, "an explicit token budget wins");
        explicit_budget.insert("thinking_level".into(), serde_json::json!("16384"));
        normalize_thinking_option(&mut explicit_budget, Some(&fixed_model));
        assert_eq!(
            explicit_budget.get("thinking_budget_tokens"),
            Some(&serde_json::json!(8192))
        );
        assert!(!explicit_budget.contains_key("thinking_level"));

        options.remove("reasoning_effort");
        options.insert("thinking_level".into(), serde_json::json!("16384"));
        normalize_thinking_option(&mut options, Some(&fixed_model));
        assert_eq!(
            options.get("thinking_budget_tokens"),
            Some(&serde_json::json!(16384))
        );
        assert_eq!(
            resolved_thinking_level(&options, Some(&fixed_model)).as_deref(),
            Some("16384")
        );

        options.remove("thinking_budget_tokens");
        options.insert("thinking_level".into(), serde_json::json!("1e4"));
        normalize_thinking_option(&mut options, Some(&fixed_model));
        assert_eq!(
            options.get("thinking_budget_tokens"),
            Some(&serde_json::json!(10000))
        );

        assert_eq!(parse_thinking_budget("1023.9999999999999999"), None);
        assert_eq!(parse_thinking_budget("1e-999"), None);
        assert_eq!(parse_thinking_budget("184467440737095516160.0"), None);
        assert_eq!(parse_thinking_budget("1.0e4"), Some(10000));
        assert_eq!(parse_thinking_budget(".1e5"), Some(10000));
        assert_eq!(parse_thinking_budget("+1024.0"), Some(1024));
        assert_eq!(parse_thinking_budget("+1.024e3"), Some(1024));
        assert_eq!(parse_thinking_budget("-1024.0"), None);

        // No thinking enum means the inherited option is not sent.
        options.remove("thinking_budget_tokens");
        options.insert("thinking_level".into(), serde_json::json!("high"));
        normalize_thinking_option(&mut options, None);
        assert!(options.is_empty());
    }

    #[tokio::test]
    async fn complete_login_forwards_callback_once() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        );
        let (callback_tx, mut callback_rx) = tokio::sync::mpsc::channel(1);
        engine.logins.lock().unwrap().insert(
            "claude-code".into(),
            LoginState::Pending {
                started: trouve_protocol::LoginStarted {
                    verification_url: "https://claude.example.test/oauth".into(),
                    user_code: None,
                },
                callback_sender: Some(callback_tx),
            },
        );

        let callback = "http://localhost:54545/callback?code=test-code&state=test-state";
        let status = engine
            .complete_login(
                "claude-code",
                trouve_protocol::CompleteLoginRequest {
                    callback_url: callback.into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(status.status, "pending");
        assert_eq!(callback_rx.recv().await.as_deref(), Some(callback));

        assert!(matches!(
            engine
                .complete_login(
                    "claude-code",
                    trouve_protocol::CompleteLoginRequest {
                        callback_url: callback.into(),
                    },
                )
                .await,
            Err(EngineError::Conflict(_))
        ));
        assert!(matches!(
            engine
                .complete_login(
                    "claude-code",
                    trouve_protocol::CompleteLoginRequest {
                        callback_url: "http://localhost/callback\ninjected".into(),
                    },
                )
                .await,
            Err(EngineError::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn session_lifecycle_shares_turns_and_excludes_destructive_operations() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        );
        let lock = engine.session_lock("se_shared");
        let first_reader = lock.read().await;
        let second_reader = lock
            .try_read()
            .expect("a second read-only turn should overlap");
        assert!(
            lock.try_write().is_err(),
            "destructive lifecycle work must wait for every active turn"
        );
        drop(second_reader);
        drop(first_reader);
        assert!(lock.try_write().is_ok());
    }

    #[test]
    fn vendor_backends_receive_their_supported_tool_bridge_surface() {
        let data = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.providers.insert(
            "codex".into(),
            ProviderConfig {
                kind: "codex-app-server".into(),
                ..Default::default()
            },
        );
        config.providers.insert(
            "claude".into(),
            ProviderConfig {
                kind: "claude-cli".into(),
                ..Default::default()
            },
        );
        config.providers.insert(
            "claude-native".into(),
            ProviderConfig {
                kind: "claude-cli".into(),
                tool_bridge: Some(false),
                ..Default::default()
            },
        );
        config.providers.insert(
            "cursor".into(),
            ProviderConfig {
                kind: "cursor-cli".into(),
                ..Default::default()
            },
        );
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &config,
        );
        engine.set_base_url("http://127.0.0.1:4000");
        let _cancel = engine.register_cancel("th_1");

        let codex = engine.mcp_bridge_for("codex/model", "th_1").unwrap();
        assert!(codex.bridge_tools);
        assert!(codex.url.contains("tools=1"));
        assert!(codex.disallowed_tools.is_empty());
        let codex_ticket = codex
            .url
            .split('?')
            .nth(1)
            .unwrap()
            .split('&')
            .find_map(|pair| pair.strip_prefix("ticket="))
            .unwrap();
        let claims = engine
            .validate_bridge_ticket(codex_ticket, "th_1", true, false)
            .unwrap();
        assert!(claims.correlate_codex_owner);
        assert!(
            engine
                .validate_bridge_ticket(codex_ticket, "th_tampered", true, false)
                .is_none()
        );
        assert!(
            engine
                .validate_bridge_ticket(codex_ticket, "th_1", false, false)
                .is_none()
        );
        assert!(
            engine
                .validate_bridge_ticket(codex_ticket, "th_1", true, true)
                .is_none()
        );

        engine.clear_cancel("th_1");
        assert!(
            engine
                .validate_bridge_ticket(codex_ticket, "th_1", true, false)
                .is_none(),
            "a retained capability must remain dormant between turns"
        );
        let _resumed_cancel = engine.register_cancel("th_1");
        let resumed_codex = engine.mcp_bridge_for("codex/model", "th_1").unwrap();
        assert_eq!(
            resumed_codex.url, codex.url,
            "a resumed vendor runtime must receive the URL its persistent MCP client already uses"
        );
        assert!(
            engine
                .validate_bridge_ticket(codex_ticket, "th_1", true, false)
                .is_some()
        );

        let claude = engine.mcp_bridge_for("claude/model", "th_1").unwrap();
        assert!(claude.bridge_tools);
        assert!(claude.disallowed_tools.iter().any(|tool| tool == "Edit"));
        assert!(
            engine
                .validate_bridge_ticket(codex_ticket, "th_1", true, false)
                .is_none(),
            "changing the bridge capability surface must rotate the old ticket"
        );

        let native = engine
            .mcp_bridge_for("claude-native/model", "th_1")
            .unwrap();
        assert!(!native.bridge_tools);
        assert!(native.url.contains("tools=0"));

        let cursor = engine.mcp_bridge_for("cursor/model", "th_1").unwrap();
        assert!(!cursor.bridge_tools);
        assert!(cursor.url.contains("tools=0"));
        assert!(cursor.url.contains("approval=0"));
        assert!(cursor.disallowed_tools.is_empty());
        engine.clear_cancel("th_1");
    }

    #[tokio::test]
    async fn no_tools_vendor_mutation_is_denied_without_approval_or_permit() {
        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let session = Session {
            id: "se_no_tools_approval".into(),
            workspace_id: "ws_no_tools_approval".into(),
            title: "No-tools approval".into(),
            branch: "trouve/no-tools-approval".into(),
            worktree_path: data.path().to_string_lossy().into_owned(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        let thread = Thread {
            id: "th_no_tools_approval".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "cursor/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        let mode = personas::find_persona(&personas::builtin_personas(), "code")
            .unwrap()
            .clone();
        let tools_enabled = false;
        let effective_read_only = !tools_enabled || mode.read_only;
        let thread_id = thread.id.clone();
        let (response, _response_rx) = tokio::sync::oneshot::channel();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            engine.pending_backend_approval(
                session,
                thread,
                1,
                effective_read_only,
                "vendor-no-tools-call".into(),
                "CommandExecution".into(),
                serde_json::json!({}),
                response,
                tokio_util::sync::CancellationToken::new(),
                true,
                None,
            ),
        )
        .await
        .expect("read-only approval gating should deny without waiting");

        assert!(!outcome.approved.unwrap());
        assert!(outcome.mutation_permit.is_none());
        assert_eq!(
            engine
                .approvals
                .resolve(&thread_id, "vendor-no-tools-call", ApprovalDecision::Deny,),
            ApprovalResolution::NotFound,
        );
    }

    #[tokio::test]
    async fn approved_vendor_mutation_waits_for_the_session_tool_lane() {
        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        ));
        let session = Session {
            id: "se_vendor_lane".into(),
            workspace_id: "ws_vendor_lane".into(),
            title: "Vendor lane".into(),
            branch: "trouve/vendor-lane".into(),
            worktree_path: data.path().to_string_lossy().into_owned(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        let thread = Thread {
            id: "th_vendor_lane".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "cursor/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Yolo,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        let read_permit = engine.tool_execution_lock(&session.id).read_owned().await;
        let (response, _response_rx) = tokio::sync::oneshot::channel();
        let mut approval = engine.pending_backend_approval(
            session,
            thread,
            1,
            false,
            "vendor-call".into(),
            "CommandExecution".into(),
            serde_json::json!({}),
            response,
            tokio_util::sync::CancellationToken::new(),
            true,
            None,
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut approval)
                .await
                .is_err(),
            "vendor mutation approval must not pass while a read permit is active"
        );
        drop(read_permit);
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), approval)
            .await
            .expect("vendor mutation should acquire the lane after release");
        assert_eq!(outcome.call_id, "vendor-call");
        assert!(outcome.approved.unwrap());
        assert!(outcome.mutation_permit.is_some());
    }

    #[tokio::test]
    async fn cancelled_native_tool_retains_execution_lane_until_cleanup_acknowledgement() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_cancel_tool".into(),
            name: "cancel tool".into(),
            path: data.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_cancel_tool".into(),
            workspace_id: workspace.id.clone(),
            title: "Cancel tool".into(),
            branch: "trouve/cancel-tool".into(),
            worktree_path: workspace.path.clone(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_cancel_tool".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "test/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Yolo,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let cleanup_started = Arc::new(tokio::sync::Semaphore::new(0));
        let cleanup_release = Arc::new(tokio::sync::Semaphore::new(0));
        let engine = Arc::new(
            Engine::new(store, data.path().into(), &Config::default()).with_executor(Arc::new(
                CancellationAwareToolExecutor {
                    started: started.clone(),
                    cleanup_started: cleanup_started.clone(),
                    cleanup_release: cleanup_release.clone(),
                },
            )),
        );
        let mode = personas::find_persona(&personas::builtin_personas(), "code")
            .unwrap()
            .clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolCtx {
            cancel: cancel.clone(),
            worktree: data.path().into(),
            thread_id: thread.id.clone(),
            ..Default::default()
        };
        let call = trouve_providers::ToolCallRequest {
            id: "call-cancel-tool".into(),
            name: "write_test".into(),
            arguments: serde_json::json!({}),
        };
        let execution = tokio::spawn({
            let engine = engine.clone();
            let session = session.clone();
            let thread = thread.clone();
            let cancel = cancel.clone();
            async move {
                engine
                    .handle_tool_call(&session, &thread, 1, &mode, &ctx, &call, &cancel)
                    .await
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            started.clone().acquire_owned(),
        )
        .await
        .expect("tool should begin")
        .unwrap()
        .forget();

        cancel.cancel();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            cleanup_started.clone().acquire_owned(),
        )
        .await
        .expect("tool should observe cancellation")
        .unwrap()
        .forget();
        assert!(!execution.is_finished());
        let lane = engine.tool_execution_lock(&session.id);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), lane.read())
                .await
                .is_err(),
            "mutation lane was released before tool cleanup completed"
        );

        cleanup_release.add_permits(1);
        let (result, images) = tokio::time::timeout(std::time::Duration::from_secs(1), execution)
            .await
            .expect("tool call should finish after cleanup acknowledgement")
            .unwrap()
            .unwrap();
        assert!(result.contains("tool cancelled after cleanup"));
        assert!(images.is_empty());

        // A misbehaving executor that exceeds the bounded acknowledgement
        // wait may no longer block the terminal turn state, but its mutation
        // lane remains quarantined until the executor really returns.
        let cancel = tokio_util::sync::CancellationToken::new();
        let ctx = ToolCtx {
            cancel: cancel.clone(),
            worktree: data.path().into(),
            thread_id: thread.id.clone(),
            ..Default::default()
        };
        let call = trouve_providers::ToolCallRequest {
            id: "call-quarantine-tool".into(),
            name: "write_test".into(),
            arguments: serde_json::json!({}),
        };
        let mode = personas::find_persona(&personas::builtin_personas(), "code")
            .unwrap()
            .clone();
        let execution = tokio::spawn({
            let engine = engine.clone();
            let session = session.clone();
            let thread = thread.clone();
            let cancel = cancel.clone();
            async move {
                engine
                    .handle_tool_call(&session, &thread, 2, &mode, &ctx, &call, &cancel)
                    .await
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            started.clone().acquire_owned(),
        )
        .await
        .expect("second tool should begin")
        .unwrap()
        .forget();
        cancel.cancel();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            cleanup_started.clone().acquire_owned(),
        )
        .await
        .expect("second tool should enter cleanup")
        .unwrap()
        .forget();
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(1), execution)
            .await
            .expect("engine should bound a non-acknowledging executor")
            .unwrap()
            .unwrap();
        assert!(result.contains("tool cancellation cleanup timed out"));
        let lane = engine.tool_execution_lock(&session.id);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), lane.read())
                .await
                .is_err(),
            "timed-out mutation lane must remain quarantined"
        );
        cleanup_release.add_permits(1);
        let _released = tokio::time::timeout(std::time::Duration::from_secs(1), lane.read())
            .await
            .expect("quarantined lane should release after late cleanup");
    }

    #[tokio::test]
    async fn native_tool_batches_overlap_reads_and_serialize_mutations() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_parallel_tools".into(),
            name: "parallel tools".into(),
            path: data.path().to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_parallel_tools".into(),
            workspace_id: workspace.id.clone(),
            title: "Parallel tools".into(),
            branch: "trouve/parallel".into(),
            worktree_path: workspace.path.clone(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_parallel_tools".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
            mode: "code".into(),
            model: "test/model".into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Yolo,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let releases = Arc::new(tokio::sync::Semaphore::new(0));
        let engine = Arc::new(
            Engine::new(store, data.path().into(), &Config::default()).with_executor(Arc::new(
                BlockingToolExecutor {
                    started: started_tx,
                    releases: releases.clone(),
                },
            )),
        );
        let mode = personas::find_persona(&personas::builtin_personas(), "code")
            .unwrap()
            .clone();
        let ctx = ToolCtx {
            worktree: data.path().into(),
            thread_id: thread.id.clone(),
            ..Default::default()
        };
        let calls = |names: &[&str]| {
            names
                .iter()
                .enumerate()
                .map(|(index, name)| trouve_providers::ToolCallRequest {
                    id: format!("call-{name}-{index}"),
                    name: (*name).to_string(),
                    arguments: serde_json::json!({}),
                })
                .collect::<Vec<_>>()
        };

        let read_batch = tokio::spawn({
            let engine = engine.clone();
            let session = session.clone();
            let thread = thread.clone();
            let mode = mode.clone();
            let ctx = ctx.clone();
            async move {
                engine
                    .handle_tool_calls_parallel(
                        &session,
                        &thread,
                        1,
                        &mode,
                        &ctx,
                        calls(&["read_one", "read_two"]),
                        &tokio_util::sync::CancellationToken::new(),
                    )
                    .await
            }
        });
        let mut started_reads = HashSet::new();
        for _ in 0..2 {
            started_reads.insert(
                tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
                    .await
                    .expect("both read-only calls should start together")
                    .unwrap(),
            );
        }
        assert_eq!(
            started_reads,
            HashSet::from(["read_one".to_string(), "read_two".to_string()])
        );
        releases.add_permits(2);
        assert!(
            read_batch
                .await
                .unwrap()
                .iter()
                .all(|(_, result)| result.is_ok())
        );

        let write_batch = tokio::spawn({
            let engine = engine.clone();
            let session = session.clone();
            let thread = thread.clone();
            let mode = mode.clone();
            let ctx = ctx.clone();
            async move {
                engine
                    .handle_tool_calls_parallel(
                        &session,
                        &thread,
                        2,
                        &mode,
                        &ctx,
                        calls(&["write_one", "write_two"]),
                        &tokio_util::sync::CancellationToken::new(),
                    )
                    .await
            }
        });
        let first_write =
            tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
                .await
                .expect("the first mutation should start")
                .unwrap();
        assert!(first_write.starts_with("write_"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), started_rx.recv())
                .await
                .is_err(),
            "a second mutation must wait for the first"
        );
        releases.add_permits(1);
        let second_write =
            tokio::time::timeout(std::time::Duration::from_secs(1), started_rx.recv())
                .await
                .expect("the second mutation should start after release")
                .unwrap();
        assert!(second_write.starts_with("write_"));
        assert_ne!(first_write, second_write);
        releases.add_permits(1);
        assert!(
            write_batch
                .await
                .unwrap()
                .iter()
                .all(|(_, result)| result.is_ok())
        );
    }

    #[tokio::test]
    async fn gpu_only_title_settings_require_a_detected_gpu_before_transition() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &Config::default(),
        );
        engine
            .hardware
            .set(crate::local::Hardware {
                ram_bytes: 16 * 1024 * 1024 * 1024,
                gpus: Vec::new(),
            })
            .unwrap();

        let _transition = engine.title_model_behavior_transition.lock().await;
        let error = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            engine.set_git_worktree_settings(
                trouve_protocol::TitleModelLoadBehavior::Always,
                trouve_protocol::TitleModelResourcePolicy::GpuOnly,
                None,
            ),
        )
        .await
        .expect("hardware validation must run before waiting for the transition lock")
        .unwrap_err();

        assert!(matches!(error, EngineError::BadRequest(ref message)
                if message == "GPU-only session naming requires a detected GPU"));
        let config = engine.config.lock().unwrap();
        assert_eq!(config.title_model_load_behavior, None);
        assert_eq!(config.title_model_resource_policy, None);
        drop(config);
        assert_eq!(
            engine.git_worktree_settings().title_model_resource_policy,
            trouve_protocol::TitleModelResourcePolicy::CpuRamOnly
        );
    }

    #[test]
    fn provider_templates_expand_without_shell_semantics() {
        let values = std::collections::BTreeMap::from([
            ("ACCOUNT".into(), "tenant-1".into()),
            ("API_KEY".into(), "secret".into()),
        ]);
        assert_eq!(
            expand_provider_template(
                "https://${ACCOUNT}.example.test/v1?literal=$HOME&key=${API_KEY}",
                &values,
            )
            .unwrap(),
            "https://tenant-1.example.test/v1?literal=$HOME&key=secret"
        );
        assert!(expand_provider_template("${MISSING}", &values).is_err());
        assert!(expand_provider_template("${BAD-NAME}", &values).is_err());

        // Only catalog-declared env fallbacks are copied into `values`;
        // arbitrary process environment variables are never template input.
        const ENV_ONLY: &str = "TROUVE_PROVIDER_TEMPLATE_ENV_ONLY_TEST";
        // Safety: this test owns a unique process variable name.
        unsafe { std::env::set_var(ENV_ONLY, "must-not-expand") };
        assert!(
            expand_provider_template("${TROUVE_PROVIDER_TEMPLATE_ENV_ONLY_TEST}", &values).is_err()
        );
        // Safety: restore the unique test variable.
        unsafe { std::env::remove_var(ENV_ONLY) };
    }

    #[test]
    fn preset_upsert_preserves_existing_transport_templates_when_omitted() {
        let data = tempfile::tempdir().unwrap();
        let custom_base_url = "https://custom.azure.test/openai".to_string();
        let custom_headers =
            std::collections::BTreeMap::from([("x-custom".into(), "${RESOURCE}".into())]);
        let custom_query =
            std::collections::BTreeMap::from([("custom".into(), "${DEPLOYMENT}".into())]);
        let mut config = Config::default();
        config.providers.insert(
            "azure".into(),
            crate::config::ProviderConfig {
                kind: "azure-openai".into(),
                base_url: Some(custom_base_url.clone()),
                headers: custom_headers.clone(),
                query_params: custom_query.clone(),
                ..Default::default()
            },
        );
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &config,
        );
        engine
            .upsert_provider(
                "azure",
                &UpsertProviderRequest {
                    kind: "azure-openai".into(),
                    ..Default::default()
                },
            )
            .unwrap();

        let config = engine.config.lock().unwrap();
        let provider = config.providers.get("azure").unwrap();
        assert_eq!(provider.base_url.as_deref(), Some(custom_base_url.as_str()));
        assert_eq!(provider.headers, custom_headers);
        assert_eq!(provider.query_params, custom_query);
    }

    #[test]
    fn provider_upsert_waits_for_in_flight_delete() {
        const ID: &str = "serialized";

        let data = tempfile::tempdir().unwrap();
        let mut config = Config {
            local_enabled: Some(false),
            ..Default::default()
        };
        config.providers.insert(
            ID.into(),
            ProviderConfig {
                kind: "openai-compat".into(),
                base_url: Some("https://old.example.test/v1".into()),
                ..Default::default()
            },
        );
        let secret_store = Arc::new(BlockingProviderSecretStore::new());
        secret_store
            .values
            .lock()
            .unwrap()
            .insert(trouve_providers::secrets::api_key_secret(ID), "old".into());
        let mut engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().to_path_buf(),
            &config,
        );
        engine.secrets = secret_store.clone();
        let engine = Arc::new(engine);

        let deleting = {
            let engine = engine.clone();
            std::thread::spawn(move || engine.delete_provider(ID))
        };
        secret_store.delete_started.wait();

        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let upserting = {
            let engine = engine.clone();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                let result = engine.upsert_provider(
                    ID,
                    &UpsertProviderRequest {
                        kind: "openai-compat".into(),
                        base_url: Some("https://new.example.test/v1".into()),
                        api_key: Some("new".into()),
                        ..Default::default()
                    },
                );
                done_tx.send(result).unwrap();
            })
        };
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));

        secret_store.allow_delete.wait();
        deleting.join().unwrap().unwrap();
        let info = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        upserting.join().unwrap();

        assert_eq!(info.id, ID);
        assert!(engine.config.lock().unwrap().providers.contains_key(ID));
        assert_eq!(
            secret_store
                .values
                .lock()
                .unwrap()
                .get(&trouve_providers::secrets::api_key_secret(ID))
                .map(String::as_str),
            Some("new")
        );
    }
}
