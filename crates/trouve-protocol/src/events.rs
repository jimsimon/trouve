//! The event log envelope and event taxonomy.
//!
//! Semantics are specified in `docs/design/event-log.md`. Clients must
//! ignore unknown event types; removing or repurposing a type is a breaking
//! protocol change.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{CallId, CheckpointId, SessionId, ThreadId, WorkspaceId};

/// Which stream an event belongs to. Cursors are monotonic per scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Server,
    Session(SessionId),
    Thread(ThreadId),
    CodeReviewJob(String),
}

/// The envelope every event is delivered in (and persisted as).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EventEnvelope {
    /// Strictly increasing within a scope; used as the SSE event id for
    /// `Last-Event-ID` resumption. Not necessarily dense.
    pub cursor: u64,
    pub scope: Scope,
    /// RFC 3339 timestamp assigned at append time.
    pub ts: chrono::DateTime<chrono::Utc>,
    #[serde(flatten)]
    pub event: Event,
}

/// Aggregate user attention required anywhere in a session. This projection
/// prevents clients from retaining every background thread history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionAttention {
    None,
    Approval,
    Question,
    Both,
}

/// Aggregate execution outcome for the session inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    Idle,
    Running,
    Succeeded,
    Failed,
}

/// A fresh session-level notification edge derived from a durable thread
/// event. Clients apply their own notification preferences and foreground
/// suppression; this type only preserves the native event category without
/// requiring one background SSE follower per thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionNotificationKind {
    TurnCompleted,
    TurnFailed,
    ApprovalRequested,
    QuestionRequested,
}

/// Durable server projection used by desktop and PWA session lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub archived: bool,
    pub active: bool,
    pub attention: SessionAttention,
    pub outcome: SessionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_thread_id: Option<ThreadId>,
    /// Cursor of the durable source event that produced this state.
    pub latest_cursor: u64,
    /// Timestamp of that source event.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Compact durable execution state for one thread. This lets clients render
/// background-thread status without retaining or streaming every transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ThreadStatus {
    pub thread_id: ThreadId,
    pub session_id: SessionId,
    pub active: bool,
    pub attention: SessionAttention,
    pub outcome: SessionOutcome,
    /// Cursor of the durable source event that produced this state.
    pub latest_cursor: u64,
    /// Start of the latest turn, when this thread has run at least once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// End of the latest turn. Absent while that turn is still active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Atomic session-summary snapshot plus the server-scope cursor after which a
/// client resumes the existing durable event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionSummariesSnapshot {
    pub summaries: Vec<SessionSummary>,
    pub cursor: u64,
}

/// Permission decision for an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    /// Approve and add to the session allow-list so equivalent calls skip
    /// future prompts.
    AlwaysApprove,
    Deny,
}

/// One choice offered by a [`Question`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
}

/// A single question inside a `question.requested` event. Clients always
/// offer a trailing free-form "Other" choice in addition to the listed
/// options; its text comes back in [`QuestionAnswer::other_text`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Question {
    pub id: String,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
    /// Multiple options may be selected (checkboxes instead of radios).
    #[serde(default)]
    pub allow_multiple: bool,
}

/// The user's answer to one [`Question`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QuestionAnswer {
    pub question_id: String,
    /// Ids of the selected options (at most one unless `allow_multiple`).
    #[serde(default)]
    pub selected_option_ids: Vec<String>,
    /// Free-form text when the user picked "Other".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_text: Option<String>,
}

/// One slash command or skill accepted by Trouve, surfaced by clients as a
/// prompt-box completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CommandInfo {
    /// Name without the leading slash.
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Whether submitting this command starts a model turn or executes a
    /// deterministic Trouve action.
    #[serde(default)]
    pub kind: CommandKind,
    /// User-facing invocation syntax, including the leading slash.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub usage: String,
}

/// How a slash command is dispatched by clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    /// Send the invocation as a prompt (skills and other model workflows).
    #[default]
    Prompt,
    /// Execute through Trouve's typed command endpoint without involving a
    /// model provider.
    Action,
}

/// Token/cost usage for a turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Usage {
    /// Non-cached input tokens. This counter is mutually exclusive with
    /// `cached_input_tokens`, even when the upstream provider reports an
    /// inclusive input total.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Cached/read tokens where the provider reports them.
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Provider-authoritative model-visible tokens for the most recent
    /// request. Unlike the aggregate turn counters above, this is the current
    /// context-size measurement used for context-window presentation and
    /// compaction decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_input_tokens: Option<u64>,
    /// Estimated cost in USD, when list pricing for the model is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The model's context window as reported live by the provider during
    /// the turn. Authoritative over any static catalog value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// Terminal status of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Ok,
    Error,
    Denied,
    Aborted,
}

/// Current user-visible startup activity for a running turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnPhase {
    Processing,
    ConnectingTools,
}

/// Every event type in the log. Serialized with a `type` tag using
/// dot-namespaced names, per the event-log design doc.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type")]
pub enum Event {
    // --- thread scope -----------------------------------------------------
    /// Shared/provider capacity has been acquired for this turn. Interactive
    /// turns use the foreground lane; unattended review tasks use background.
    #[serde(rename = "turn.capacity_acquired")]
    TurnCapacityAcquired {
        turn: u64,
        wait_ms: u64,
        background: bool,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        turn: u64,
        mode: String,
        model: String,
        /// Effective provider-native thinking/reasoning selection for this
        /// turn after inherited defaults and model schema normalization.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_level: Option<String>,
        /// Whether the backend running this exact turn accepts additional
        /// user input without cancelling or starting another turn.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        supports_steering: bool,
    },
    /// The transient activity label for a running turn changed. This updates
    /// the existing activity row; it is not a transcript or tool-rail item.
    #[serde(rename = "turn.phase_changed")]
    TurnPhaseChanged { turn: u64, phase: TurnPhase },
    /// Live usage from the most recently completed model request in a running
    /// turn. Thread snapshots add its billing counters to `active_usage` while
    /// replacing only the context fields. `last_usage` remains unchanged until
    /// `turn.completed` supplies the turn's final aggregates.
    #[serde(rename = "turn.usage_updated")]
    TurnUsageUpdated { turn: u64, usage: Usage },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        turn: u64,
        usage: Usage,
        #[serde(skip_serializing_if = "Option::is_none")]
        checkpoint_id: Option<CheckpointId>,
    },
    #[serde(rename = "turn.failed")]
    TurnFailed { turn: u64, error: String },
    /// The turn was interrupted by the user (via the cancel endpoint). Like
    /// `turn.failed` it pauses the queue, but it isn't an error condition.
    #[serde(rename = "turn.cancelled")]
    TurnCancelled { turn: u64 },

    #[serde(rename = "user.message")]
    UserMessage {
        turn: u64,
        content: String,
        /// Files the user attached to the prompt (bytes at
        /// `GET /v1/attachments/{id}`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::Attachment>,
    },
    /// Additional user input accepted by the backend while `turn` was still
    /// running. This belongs on the active turn's timeline and does not start
    /// or queue another turn.
    #[serde(rename = "turn.steered")]
    TurnSteered {
        turn: u64,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::Attachment>,
    },
    /// A child agent became part of this parent turn. The child transcript
    /// remains independently addressable while this durable boundary lets
    /// clients render and navigate the relationship from the parent rail.
    #[serde(rename = "subagent.spawned")]
    SubagentSpawned {
        turn: u64,
        thread_id: ThreadId,
        session_id: SessionId,
        prompt: String,
        model: String,
        /// The trouve spawn tool call represented by this node, when one
        /// exists. Provider-native collaborators do not have a parent tool
        /// call. Clients may suppress the redundant spawn tool presentation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<CallId>,
    },
    /// Streamed model output. Replaying all deltas of a turn reproduces the
    /// final message exactly.
    #[serde(rename = "assistant.delta")]
    AssistantDelta { turn: u64, text: String },
    /// Streamed user-facing progress authored by the agent harness. Progress
    /// is distinct from both model reasoning and answer text.
    #[serde(rename = "assistant.progress")]
    AssistantProgress { turn: u64, text: String },
    /// The harness explicitly closed the current progress item.
    #[serde(rename = "assistant.progress_completed")]
    AssistantProgressCompleted { turn: u64 },
    /// Streamed model reasoning ("thinking") text, where the provider
    /// exposes it. Display-only: never part of the provider transcript.
    #[serde(rename = "assistant.thinking")]
    AssistantThinking { turn: u64, text: String },
    /// The provider explicitly closed the current streamed thinking item.
    /// This boundary can arrive before the next visible assistant or tool
    /// event, so clients must not infer it from subsequent output alone.
    #[serde(rename = "assistant.thinking_completed")]
    AssistantThinkingCompleted { turn: u64 },
    /// Folded final assistant text for the turn.
    #[serde(rename = "assistant.message")]
    AssistantMessage { turn: u64, content: String },

    #[serde(rename = "tool.requested")]
    ToolRequested {
        turn: u64,
        call_id: CallId,
        tool: String,
        args: serde_json::Value,
        requires_approval: bool,
    },
    #[serde(rename = "approval.requested")]
    ApprovalRequested { turn: u64, call_id: CallId },
    #[serde(rename = "approval.resolved")]
    ApprovalResolved {
        call_id: CallId,
        decision: ApprovalDecision,
    },
    #[serde(rename = "tool.started")]
    ToolStarted { call_id: CallId },
    #[serde(rename = "tool.output")]
    ToolOutput { call_id: CallId, chunk: String },
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        call_id: CallId,
        status: ToolStatus,
        result: serde_json::Value,
        /// Time spent inside `ToolExecutor::execute`, measured with a
        /// monotonic clock. Absent for older servers, denied calls, and
        /// provider-owned tool calls that do not expose an execution span.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_duration_ms: Option<u64>,
    },

    /// The agent asked the user one or more questions; the turn is blocked
    /// until `question.resolved`. Clients render an answer wizard.
    #[serde(rename = "question.requested")]
    QuestionRequested {
        turn: u64,
        request_id: CallId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        questions: Vec<Question>,
    },
    /// Answers submitted (or `answers: null` when the user skipped).
    #[serde(rename = "question.resolved")]
    QuestionResolved {
        request_id: CallId,
        #[serde(default)]
        answers: Option<Vec<QuestionAnswer>>,
    },

    /// Trouve's authoritative slash-command and skill catalog for this
    /// thread. Replaces any previously announced catalog.
    #[serde(rename = "thread.command_catalog_updated")]
    CommandCatalogUpdated { commands: Vec<CommandInfo> },

    /// A deterministic Trouve command completed. The output is persisted so
    /// replay and every client render the same command history.
    #[serde(rename = "thread.command_executed")]
    CommandExecuted {
        name: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        arguments: String,
        output: String,
    },

    /// Legacy vendor-reported slash commands / skills. Kept for replay of
    /// existing event logs; new turns publish CommandCatalogUpdated instead.
    #[serde(rename = "thread.commands_updated")]
    CommandsUpdated { commands: Vec<CommandInfo> },

    /// The thread's queue of pending prompts changed (enqueue, edit,
    /// reorder, delete, or dispatch). Carries the full remaining queue in
    /// run order; clients replace any previous list.
    #[serde(rename = "thread.queue_updated")]
    QueueUpdated { prompts: Vec<crate::QueuedPrompt> },

    /// The thread's current todo snapshot changed. Historical `todo_write`
    /// tool calls remain in the stream; clients replace this snapshot.
    #[serde(rename = "thread.todos_updated")]
    TodosUpdated { todos: Vec<crate::TodoItem> },

    /// The thread's transcript neared the model's context window; the engine
    /// is summarizing older messages. Clients show a busy indicator.
    #[serde(rename = "thread.compaction_started")]
    CompactionStarted { turn: u64 },
    #[serde(rename = "thread.compaction_completed")]
    CompactionCompleted {
        turn: u64,
        /// Provider-transcript messages folded into the summary. Zero means
        /// an external harness reported the boundary without a message count.
        messages_compacted: u64,
    },
    /// A provider-owned compaction item terminated unsuccessfully. This is a
    /// distinct terminal edge so clients can clear their busy state even when
    /// the vendor turn continues producing ordinary output.
    #[serde(rename = "thread.compaction_failed")]
    CompactionFailed { turn: u64 },

    // --- session scope ----------------------------------------------------
    #[serde(rename = "checkpoint.created")]
    CheckpointCreated {
        checkpoint_id: CheckpointId,
        thread_id: ThreadId,
        turn: u64,
        /// Git commit hash the checkpoint points at.
        commit: String,
    },
    #[serde(rename = "checkpoint.restored")]
    CheckpointRestored {
        checkpoint_id: CheckpointId,
        direction: RestoreDirection,
    },
    #[serde(rename = "worktree.created")]
    WorktreeCreated { path: String, branch: String },
    #[serde(rename = "worktree.removed")]
    WorktreeRemoved { path: String, branch: String },

    // --- code-review-job scope ------------------------------------------
    /// A router, reviewer, or coordinator task changed durable state.
    #[serde(rename = "code_review.task_updated")]
    CodeReviewTaskUpdated {
        job_id: String,
        task: Box<crate::CodeReviewTask>,
    },
    /// A compact task lifecycle/metrics snapshot changed while the task was
    /// running. Clients merge it into their retained task representation.
    #[serde(rename = "code_review.task_progress_updated")]
    CodeReviewTaskProgressUpdated {
        job_id: String,
        task_id: String,
        progress: crate::CodeReviewTaskProgress,
    },
    /// The complete, durable persona-routing matrix was selected for a job.
    #[serde(rename = "code_review.routing_updated")]
    CodeReviewRoutingUpdated {
        job_id: String,
        routing_decisions: Vec<crate::CodeReviewRoutingDecision>,
    },
    /// Live output projected from the disposable agent thread into durable
    /// review history.
    #[serde(rename = "code_review.output_delta")]
    CodeReviewOutputDelta {
        job_id: String,
        task_id: String,
        stream: crate::CodeReviewOutputStream,
        text: String,
    },
    /// Reviewer-level progress changed. Coordinator/summary work is exposed
    /// on its task but does not inflate the reviewer count.
    #[serde(rename = "code_review.progress_updated")]
    CodeReviewProgressUpdated {
        job_id: String,
        progress: crate::CodeReviewProgress,
    },
    /// Other durable job state changed.
    #[serde(rename = "code_review.job_updated")]
    CodeReviewJobUpdated { job_id: String },

    // --- server scope -----------------------------------------------------
    #[serde(rename = "workspace.registered")]
    WorkspaceRegistered {
        workspace_id: WorkspaceId,
        path: String,
    },
    /// An account-centric PR-dashboard refresh completed for one GitHub
    /// instance. Clients replace the previously folded host slice.
    #[serde(rename = "github.pull_requests_updated")]
    GithubPullRequestsUpdated { pull_requests: crate::GithubPrList },
    #[serde(rename = "workspace.closed")]
    WorkspaceClosed { workspace_id: WorkspaceId },
    #[serde(rename = "session.created")]
    SessionCreated {
        session_id: SessionId,
        workspace_id: WorkspaceId,
    },
    #[serde(rename = "session.pr_opened")]
    SessionPrOpened { number: u64, url: String },
    #[serde(rename = "session.deleted")]
    SessionDeleted {
        session_id: SessionId,
        workspace_id: WorkspaceId,
    },
    /// Session metadata changed (rename / archive). Clients refetch.
    #[serde(rename = "session.updated")]
    SessionUpdated {
        session_id: SessionId,
        workspace_id: WorkspaceId,
    },
    #[serde(rename = "thread.created")]
    ThreadCreated {
        thread_id: ThreadId,
        session_id: SessionId,
    },
    /// Thread settings changed (mode/model). Clients refetch.
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        thread_id: ThreadId,
        session_id: SessionId,
    },
    /// Transactionally derived status for one thread. Open and closed tabs can
    /// fold this server-scope event without opening a transcript SSE stream.
    #[serde(rename = "thread.status_updated")]
    ThreadStatusUpdated { status: ThreadStatus },
    /// A session started or stopped actively processing prompts (one of its
    /// threads began running turns, or the last active one went idle).
    /// Drives the activity indicator in session lists; `Session.active`
    /// carries the same state for initial fetches.
    #[serde(rename = "session.activity")]
    SessionActivity {
        session_id: SessionId,
        workspace_id: WorkspaceId,
        active: bool,
    },
    /// The server restarted while this session still had process-owned turn
    /// state. Clients clear running/approval/question UI from the replacement
    /// summary; those responders cannot survive the process that owned them.
    #[serde(rename = "session.recovered")]
    SessionRecovered {
        session_id: SessionId,
        workspace_id: WorkspaceId,
    },
    /// Transactionally derived aggregate state. `summary: null` is the
    /// durable tombstone for a deleted session.
    #[serde(rename = "session.summary_updated")]
    SessionSummaryUpdated {
        session_id: SessionId,
        #[serde(default)]
        #[schema(required = true, nullable = true)]
        summary: Option<SessionSummary>,
    },
    /// Compact durable edge for notifications about inactive/background
    /// threads. It is appended transactionally after the replacement session
    /// summary produced by the same source event.
    #[serde(rename = "session.notification")]
    SessionNotification {
        session_id: SessionId,
        thread_id: ThreadId,
        kind: SessionNotificationKind,
        /// Optional native-equivalent failure excerpt or question subtitle.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A scheduled automation ran (or failed to). Clients refetch the
    /// automations list — and the sessions list when it succeeded, since a
    /// run creates a session.
    #[serde(rename = "automation.fired")]
    AutomationFired {
        automation_id: String,
        /// Session the run created (absent when the run failed).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
        /// Failure reason ("" = success).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        error: String,
    },
    /// GitHub App configuration, repository policy, or a durable review job
    /// changed. Clients refetch `/v1/code-review` and fold the replacement.
    #[serde(rename = "code_review.updated")]
    CodeReviewUpdated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        job_id: Option<String>,
    },
    /// The server's internet reachability changed (it is the one talking to
    /// model vendors, so it owns this state). While offline, `/v1/models`
    /// lists only models that can run without internet (local provider,
    /// loopback endpoints); clients gate prompt entry on having usable
    /// models and announce recovery. `ServerInfo.online` carries the same
    /// state for initial fetches.
    #[serde(rename = "server.connectivity_changed")]
    ConnectivityChanged { online: bool },
    /// The persisted session-naming settings or the session-title model's
    /// install/load state changed. Carries a full replacement snapshot so
    /// replay and reconnect reconstruct the settings UI exactly.
    #[serde(rename = "settings.git_worktrees_updated")]
    GitWorktreeSettingsUpdated {
        settings: crate::GitWorktreeSettings,
    },
    /// The persisted automated code-review execution deadlines changed.
    /// Carries a full replacement snapshot for replay and reconnect.
    #[serde(rename = "settings.code_review_updated")]
    CodeReviewSettingsUpdated { settings: crate::CodeReviewSettings },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDirection {
    Undo,
    Redo,
    /// Jump directly to a named checkpoint rather than taking a relative
    /// undo-stack step.
    Exact,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_tags_are_dot_namespaced() {
        let ev = Event::AssistantDelta {
            turn: 1,
            text: "hi".into(),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "assistant.delta");

        let review = serde_json::to_value(Event::CodeReviewUpdated {
            job_id: Some("rv_1".into()),
        })
        .unwrap();
        assert_eq!(review["type"], "code_review.updated");

        let progress = serde_json::to_value(Event::CodeReviewProgressUpdated {
            job_id: "rv_1".into(),
            progress: crate::CodeReviewProgress {
                completed_reviewers: 1,
                total_reviewers: 2,
                percent: 50,
            },
        })
        .unwrap();
        assert_eq!(progress["type"], "code_review.progress_updated");

        let routing = serde_json::to_value(Event::CodeReviewRoutingUpdated {
            job_id: "rv_1".into(),
            routing_decisions: vec![crate::CodeReviewRoutingDecision {
                batch_index: 0,
                reviewer_id: "concurrency".into(),
                reviewer_name: "Concurrency".into(),
                selected: true,
                reasons: vec![crate::CodeReviewRoutingReason {
                    source: crate::CodeReviewRoutingSource::Deterministic,
                    detail: "synchronization changed".into(),
                }],
            }],
        })
        .unwrap();
        assert_eq!(routing["type"], "code_review.routing_updated");
        assert_eq!(
            routing["routing_decisions"][0]["reviewer_id"],
            "concurrency"
        );
    }

    #[test]
    fn session_summary_tombstone_serializes_explicit_null() {
        let value = serde_json::to_value(Event::SessionSummaryUpdated {
            session_id: "se_deleted".into(),
            summary: None,
        })
        .unwrap();
        assert_eq!(value["type"], "session.summary_updated");
        assert!(value.get("summary").is_some());
        assert!(value["summary"].is_null());
    }

    #[test]
    fn session_notification_serializes_optional_detail() {
        let value = serde_json::to_value(Event::SessionNotification {
            session_id: "se_1".into(),
            thread_id: "th_1".into(),
            kind: SessionNotificationKind::TurnFailed,
            detail: Some("provider unavailable".into()),
        })
        .unwrap();
        assert_eq!(value["type"], "session.notification");
        assert_eq!(value["kind"], "turn_failed");
        assert_eq!(value["detail"], "provider unavailable");
    }

    #[test]
    fn github_pull_request_snapshot_roundtrips() {
        let event = Event::GithubPullRequestsUpdated {
            pull_requests: crate::GithubPrList {
                viewer: "octocat".into(),
                host: "github.com".into(),
                prs: Vec::new(),
            },
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "github.pull_requests_updated");
        let decoded: Event = serde_json::from_value(value).unwrap();
        match decoded {
            Event::GithubPullRequestsUpdated { pull_requests } => {
                assert_eq!(pull_requests.viewer, "octocat");
                assert!(pull_requests.prs.is_empty());
            }
            _ => panic!("wrong event variant"),
        }
    }

    #[test]
    fn git_worktree_settings_event_uses_namespaced_tag() {
        let event = Event::GitWorktreeSettingsUpdated {
            settings: crate::GitWorktreeSettings {
                derive_branch_name_from_session_title: false,
                title_model_load_behavior: crate::TitleModelLoadBehavior::Off,
                title_model_resource_policy: crate::TitleModelResourcePolicy::CpuRamOnly,
                title_model: crate::TitleModelStatus {
                    state: "stopped".into(),
                    detail: "Built-in naming heuristics are active.".into(),
                    runtime_installed: false,
                    model_downloaded: false,
                    install_stage: String::new(),
                    install_bytes: 0,
                    install_total: 0,
                },
            },
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "settings.git_worktrees_updated");
        assert_eq!(
            value["settings"]["derive_branch_name_from_session_title"],
            false
        );
        assert_eq!(value["settings"]["title_model_load_behavior"], "off");
        assert_eq!(
            value["settings"]["title_model_resource_policy"],
            "cpu_ram_only"
        );
    }

    #[test]
    fn code_review_settings_event_uses_namespaced_tag() {
        let event = Event::CodeReviewSettingsUpdated {
            settings: crate::CodeReviewSettings {
                max_parallel_reviews: 4,
                total_timeout_seconds: 900,
                reviewer_timeout_seconds: 600,
                coordinator_timeout_seconds: 300,
            },
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "settings.code_review_updated");
        assert_eq!(value["settings"]["max_parallel_reviews"], 4);
        assert_eq!(value["settings"]["total_timeout_seconds"], 900);
        assert_eq!(value["settings"]["reviewer_timeout_seconds"], 600);
        assert_eq!(value["settings"]["coordinator_timeout_seconds"], 300);
    }

    #[test]
    fn historical_code_review_settings_events_default_parallelism() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "type": "settings.code_review_updated",
            "settings": {
                "total_timeout_seconds": 900,
                "reviewer_timeout_seconds": 600,
                "coordinator_timeout_seconds": 300
            }
        }))
        .unwrap();
        assert!(matches!(
            event,
            Event::CodeReviewSettingsUpdated { settings }
                if settings.max_parallel_reviews == 2
        ));
    }

    #[test]
    fn command_catalog_has_its_own_wire_event() {
        let ev = Event::CommandCatalogUpdated {
            commands: vec![CommandInfo {
                name: "review".into(),
                description: "Review the current changes".into(),
                kind: CommandKind::Prompt,
                usage: "/review [request]".into(),
            }],
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "thread.command_catalog_updated");
        assert_eq!(v["commands"][0]["name"], "review");
    }

    #[test]
    fn old_command_info_defaults_to_prompt() {
        let command: CommandInfo = serde_json::from_value(serde_json::json!({
            "name": "review",
            "description": "Review changes"
        }))
        .unwrap();
        assert_eq!(command.kind, CommandKind::Prompt);
        assert!(command.usage.is_empty());
    }

    #[test]
    fn command_execution_has_a_stable_wire_event() {
        let event = Event::CommandExecuted {
            name: "status".into(),
            arguments: String::new(),
            output: "Ready".into(),
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "thread.command_executed");
        assert_eq!(value["name"], "status");
    }

    #[test]
    fn envelope_roundtrips() {
        let env = EventEnvelope {
            cursor: 42,
            scope: Scope::Thread("th_1".into()),
            ts: chrono::Utc::now(),
            event: Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "gpt-x".into(),
                thinking_level: Some("high".into()),
                supports_steering: true,
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cursor, 42);
        assert_eq!(back.scope, Scope::Thread("th_1".into()));
        assert!(matches!(
            back.event,
            Event::TurnStarted {
                thinking_level: Some(level),
                supports_steering: true,
                ..
            } if level == "high"
        ));
    }

    #[test]
    fn historical_turn_started_defaults_additive_turn_capabilities() {
        let event: Event = serde_json::from_value(serde_json::json!({
            "type": "turn.started",
            "turn": 1,
            "mode": "code",
            "model": "gpt-x"
        }))
        .unwrap();
        assert!(matches!(
            event,
            Event::TurnStarted {
                thinking_level: None,
                supports_steering: false,
                ..
            }
        ));
    }

    #[test]
    fn steered_event_omits_empty_attachments_and_roundtrips() {
        let event = Event::TurnSteered {
            turn: 9,
            content: "Focus on the failing test.".into(),
            attachments: Vec::new(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "turn.steered");
        assert!(value.get("attachments").is_none());
        assert!(matches!(
            serde_json::from_value::<Event>(value).unwrap(),
            Event::TurnSteered {
                turn: 9,
                content,
                attachments,
            } if content == "Focus on the failing test." && attachments.is_empty()
        ));
    }
}
