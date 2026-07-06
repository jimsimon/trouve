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

    #[serde(rename = "user.message")]
    UserMessage { turn: u64, content: String },
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
