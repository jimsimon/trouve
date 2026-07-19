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

/// One slash command / skill the vendor harness accepts in prompts (e.g.
/// "/simplify"), surfaced by clients as prompt-box completions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CommandInfo {
    /// Name without the leading slash.
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Token/cost usage for a turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Cached/read tokens where the provider reports them.
    #[serde(default)]
    pub cached_input_tokens: u64,
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

/// Every event type in the log. Serialized with a `type` tag using
/// dot-namespaced names, per the event-log design doc.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type")]
pub enum Event {
    // --- thread scope -----------------------------------------------------
    #[serde(rename = "turn.started")]
    TurnStarted {
        turn: u64,
        mode: String,
        model: String,
    },
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
    /// Streamed model output. Replaying all deltas of a turn reproduces the
    /// final message exactly.
    #[serde(rename = "assistant.delta")]
    AssistantDelta { turn: u64, text: String },
    /// Streamed model reasoning ("thinking") text, where the provider
    /// exposes it. Display-only: never part of the provider transcript.
    #[serde(rename = "assistant.thinking")]
    AssistantThinking { turn: u64, text: String },
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

    /// The slash commands / skills the vendor harness currently accepts in
    /// prompts. Replaces any previously announced list for the thread.
    #[serde(rename = "thread.commands_updated")]
    CommandsUpdated { commands: Vec<CommandInfo> },

    /// The thread's queue of pending prompts changed (enqueue, edit,
    /// reorder, delete, or dispatch). Carries the full remaining queue in
    /// run order; clients replace any previous list.
    #[serde(rename = "thread.queue_updated")]
    QueueUpdated { prompts: Vec<crate::QueuedPrompt> },

    /// The thread's transcript neared the model's context window; the engine
    /// is summarizing older messages. Clients show a busy indicator.
    #[serde(rename = "thread.compaction_started")]
    CompactionStarted { turn: u64 },
    #[serde(rename = "thread.compaction_completed")]
    CompactionCompleted {
        turn: u64,
        /// Provider-transcript messages folded into the summary.
        messages_compacted: u64,
    },

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

    // --- server scope -----------------------------------------------------
    #[serde(rename = "workspace.registered")]
    WorkspaceRegistered {
        workspace_id: WorkspaceId,
        path: String,
    },
    /// A workspace PR-dashboard refresh completed. This is a full snapshot
    /// for that workspace; clients replace the previously folded slice.
    #[serde(rename = "workspace.pull_requests_updated")]
    WorkspacePullRequestsUpdated {
        workspace_id: WorkspaceId,
        pull_requests: crate::WorkspacePrList,
    },
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
    /// The server's internet reachability changed (it is the one talking to
    /// model vendors, so it owns this state). While offline, `/v1/models`
    /// lists only models that can run without internet (local provider,
    /// loopback endpoints); clients gate prompt entry on having usable
    /// models and announce recovery. `ServerInfo.online` carries the same
    /// state for initial fetches.
    #[serde(rename = "server.connectivity_changed")]
    ConnectivityChanged { online: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDirection {
    Undo,
    Redo,
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
    }

    #[test]
    fn workspace_pull_request_snapshot_roundtrips() {
        let event = Event::WorkspacePullRequestsUpdated {
            workspace_id: "ws_1".into(),
            pull_requests: crate::WorkspacePrList {
                viewer: "octocat".into(),
                prs: Vec::new(),
            },
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "workspace.pull_requests_updated");
        let decoded: Event = serde_json::from_value(value).unwrap();
        match decoded {
            Event::WorkspacePullRequestsUpdated {
                workspace_id,
                pull_requests,
            } => {
                assert_eq!(workspace_id, "ws_1");
                assert_eq!(pull_requests.viewer, "octocat");
                assert!(pull_requests.prs.is_empty());
            }
            _ => panic!("wrong event variant"),
        }
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
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cursor, 42);
        assert_eq!(back.scope, Scope::Thread("th_1".into()));
    }
}
