//! Fold durable thread events into a protocol-level current-state snapshot.
//! The event log remains authoritative; this projection is rebuildable.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use trouve_protocol::{
    ApprovalDecision, Event, EventEnvelope, ThreadCompactionState, ThreadToolStatus,
    ThreadTurnState, ThreadViewItem, ThreadViewSnapshot, ToolStatus,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ThreadProjection {
    pub cursor: u64,
    pub snapshot: ThreadViewSnapshot,
    /// Execution anchors are projection state rather than protocol state.
    /// They are serialized with the cache so a command that spans a cache
    /// refresh still receives an accurate duration when it completes.
    #[serde(default)]
    tool_started_at: HashMap<String, chrono::DateTime<chrono::Utc>>,
    #[serde(skip)]
    indexes: ProjectionIndexes,
}

#[derive(Debug, Default)]
struct ProjectionIndexes {
    ready: bool,
    open_assistant: HashMap<u64, usize>,
    open_thinking: HashMap<u64, usize>,
    tools: HashMap<String, usize>,
    turns: HashMap<u64, usize>,
    open_compactions: HashMap<u64, usize>,
    questions: HashMap<String, usize>,
    latest_thinking: Option<usize>,
}

impl ThreadProjection {
    pub fn apply(&mut self, envelope: &EventEnvelope) {
        self.ensure_indexes();
        self.cursor = envelope.cursor;
        match &envelope.event {
            Event::TurnStarted {
                turn,
                model,
                thinking_level,
                ..
            } => {
                self.snapshot.turn_running = true;
                self.snapshot.turn_models.insert(*turn, model.clone());
                if let Some(thinking_level) = thinking_level {
                    self.snapshot
                        .turn_thinking_levels
                        .insert(*turn, thinking_level.clone());
                }
                self.snapshot.turn_started_at.insert(*turn, envelope.ts);
                let idx = self.push(ThreadViewItem::TurnStatus {
                    turn: *turn,
                    state: ThreadTurnState::Running,
                });
                self.indexes.turns.insert(*turn, idx);
            }
            Event::CompactionStarted { turn } => {
                self.snapshot.compacting = true;
                let idx = self.push(ThreadViewItem::Compaction {
                    turn: *turn,
                    state: ThreadCompactionState::Running,
                });
                self.indexes.open_compactions.insert(*turn, idx);
            }
            Event::CommandsUpdated { commands } => self.snapshot.commands = commands.clone(),
            Event::QueueUpdated { prompts } => self.snapshot.queue = prompts.clone(),
            Event::TodosUpdated { todos } => self.snapshot.todos = todos.clone(),
            Event::CompactionCompleted {
                turn,
                messages_compacted,
            } => {
                self.snapshot.compacting = false;
                let state = ThreadCompactionState::Completed {
                    messages_compacted: *messages_compacted,
                };
                if let Some(idx) = self.indexes.open_compactions.remove(turn) {
                    self.snapshot.items[idx] = ThreadViewItem::Compaction { turn: *turn, state };
                } else {
                    // A projection cache written by an older protocol may
                    // contain the busy flag without its new transcript row.
                    self.push(ThreadViewItem::Compaction { turn: *turn, state });
                }
            }
            Event::CompactionFailed { turn } => {
                self.snapshot.compacting = false;
                let state = ThreadCompactionState::Failed;
                if let Some(idx) = self.indexes.open_compactions.remove(turn) {
                    self.snapshot.items[idx] = ThreadViewItem::Compaction { turn: *turn, state };
                } else {
                    self.push(ThreadViewItem::Compaction { turn: *turn, state });
                }
            }
            Event::UserMessage {
                turn,
                content,
                attachments,
            } => {
                self.push(ThreadViewItem::User {
                    turn: *turn,
                    content: content.clone(),
                    attachments: attachments.clone(),
                });
            }
            Event::AssistantThinking { turn, text } => {
                self.fail_open_compaction(*turn);
                self.snapshot.thinking = true;
                if let Some(&idx) = self.indexes.open_thinking.get(turn) {
                    if let ThreadViewItem::Thinking { content, .. } = &mut self.snapshot.items[idx]
                    {
                        content.push_str(text);
                    }
                } else {
                    let idx = self.push(ThreadViewItem::Thinking {
                        turn: *turn,
                        content: text.clone(),
                        complete: false,
                    });
                    self.indexes.open_thinking.insert(*turn, idx);
                    self.indexes.latest_thinking = Some(idx);
                }
            }
            Event::AssistantThinkingCompleted { .. } => {
                self.finish_thinking();
            }
            Event::AssistantDelta { turn, text } => {
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                if let Some(&idx) = self.indexes.open_assistant.get(turn) {
                    if let ThreadViewItem::Assistant { content, .. } = &mut self.snapshot.items[idx]
                    {
                        content.push_str(text);
                    }
                } else {
                    let idx = self.push(ThreadViewItem::Assistant {
                        turn: *turn,
                        content: text.clone(),
                        complete: false,
                    });
                    self.indexes.open_assistant.insert(*turn, idx);
                }
            }
            Event::AssistantMessage { turn, content } => {
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                if let Some(idx) = self.indexes.open_assistant.remove(turn) {
                    self.snapshot.items[idx] = ThreadViewItem::Assistant {
                        turn: *turn,
                        content: content.clone(),
                        complete: true,
                    };
                } else {
                    self.push(ThreadViewItem::Assistant {
                        turn: *turn,
                        content: content.clone(),
                        complete: true,
                    });
                }
            }
            Event::ToolRequested {
                turn,
                call_id,
                tool,
                args,
                requires_approval,
                ..
            } => {
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                let idx = self.push(ThreadViewItem::ToolCall {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    args: args.clone(),
                    status: if *requires_approval {
                        ThreadToolStatus::AwaitingApproval
                    } else {
                        ThreadToolStatus::Running
                    },
                    result: None,
                    duration_ms: None,
                });
                self.indexes.tools.insert(call_id.clone(), idx);
                if !requires_approval {
                    // ToolRequested is the best fallback for backends that do
                    // not emit a distinct ToolStarted event. A later start
                    // replaces this anchor.
                    self.tool_started_at.insert(call_id.clone(), envelope.ts);
                }
            }
            Event::ApprovalRequested { call_id, .. } => {
                if !self.snapshot.pending_approvals.contains(call_id) {
                    self.snapshot.pending_approvals.push(call_id.clone());
                }
                if let Some(ThreadViewItem::ToolCall { status, .. }) = self.tool_mut(call_id) {
                    *status = ThreadToolStatus::AwaitingApproval;
                }
            }
            Event::ApprovalResolved { call_id, decision } => {
                self.snapshot.pending_approvals.retain(|id| id != call_id);
                if let Some(ThreadViewItem::ToolCall { status, .. }) = self.tool_mut(call_id) {
                    *status = if *decision == ApprovalDecision::Deny {
                        ThreadToolStatus::Denied
                    } else {
                        ThreadToolStatus::Running
                    };
                }
                if *decision != ApprovalDecision::Deny {
                    // Approval resolution is the execution boundary for
                    // backends that do not emit ToolStarted. A later explicit
                    // start replaces this fallback anchor.
                    self.tool_started_at
                        .entry(call_id.clone())
                        .or_insert(envelope.ts);
                }
            }
            Event::ToolStarted { call_id } => {
                let mut started = false;
                if let Some(ThreadViewItem::ToolCall { status, .. }) = self.tool_mut(call_id) {
                    let terminal = matches!(
                        *status,
                        ThreadToolStatus::Ok
                            | ThreadToolStatus::Error
                            | ThreadToolStatus::Denied
                            | ThreadToolStatus::Aborted
                    );
                    if !terminal && *status != ThreadToolStatus::AwaitingApproval {
                        *status = ThreadToolStatus::Running;
                        started = true;
                    }
                }
                if started {
                    self.tool_started_at.insert(call_id.clone(), envelope.ts);
                }
            }
            Event::ToolCompleted {
                call_id,
                status,
                result,
            } => {
                let measured_duration_ms = self
                    .tool_started_at
                    .remove(call_id)
                    .map(|started| (envelope.ts - started).num_milliseconds().max(0) as u64);
                if let Some(ThreadViewItem::ToolCall {
                    status: current,
                    result: current_result,
                    duration_ms,
                    ..
                }) = self.tool_mut(call_id)
                {
                    if *current != ThreadToolStatus::Denied {
                        *current = match status {
                            ToolStatus::Ok => ThreadToolStatus::Ok,
                            ToolStatus::Error => ThreadToolStatus::Error,
                            ToolStatus::Denied => ThreadToolStatus::Denied,
                            ToolStatus::Aborted => ThreadToolStatus::Aborted,
                        };
                    }
                    *current_result = Some(result.clone());
                    if measured_duration_ms.is_some() {
                        *duration_ms = measured_duration_ms;
                    }
                }
                self.snapshot.pending_approvals.retain(|id| id != call_id);
            }
            Event::QuestionRequested {
                turn,
                request_id,
                title,
                questions,
                ..
            } => {
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                if !self.snapshot.pending_questions.contains(request_id) {
                    self.snapshot.pending_questions.push(request_id.clone());
                }
                let idx = self.push(ThreadViewItem::Questions {
                    request_id: request_id.clone(),
                    title: title.clone(),
                    questions: questions.clone(),
                    resolved: false,
                    answers: None,
                });
                self.indexes.questions.insert(request_id.clone(), idx);
            }
            Event::QuestionResolved {
                request_id,
                answers,
            } => {
                self.snapshot
                    .pending_questions
                    .retain(|id| id != request_id);
                if let Some(&idx) = self.indexes.questions.get(request_id)
                    && let ThreadViewItem::Questions {
                        resolved,
                        answers: current,
                        ..
                    } = &mut self.snapshot.items[idx]
                {
                    *resolved = true;
                    *current = answers.clone();
                }
            }
            Event::TurnUsageUpdated { usage, .. } => {
                self.snapshot.last_usage = Some(usage.clone());
            }
            Event::TurnCompleted {
                turn,
                usage,
                checkpoint_id,
            } => {
                self.finish_turn(*turn, envelope.ts);
                self.snapshot.last_usage = Some(usage.clone());
                if let Some(&idx) = self.indexes.turns.get(turn) {
                    self.snapshot.items[idx] = ThreadViewItem::TurnStatus {
                        turn: *turn,
                        state: ThreadTurnState::Completed {
                            usage: usage.clone(),
                            checkpoint_id: checkpoint_id.clone(),
                        },
                    };
                }
            }
            Event::TurnFailed { turn, error } => {
                self.finish_turn(*turn, envelope.ts);
                if let Some(&idx) = self.indexes.turns.get(turn) {
                    self.snapshot.items[idx] = ThreadViewItem::TurnStatus {
                        turn: *turn,
                        state: ThreadTurnState::Failed {
                            error: error.clone(),
                        },
                    };
                }
            }
            Event::TurnCancelled { turn } => {
                self.finish_turn(*turn, envelope.ts);
                if let Some(idx) = self.indexes.turns.remove(turn) {
                    self.snapshot.items.remove(idx);
                    self.indexes = ProjectionIndexes::default();
                }
            }
            _ => {}
        }
    }

    fn push(&mut self, item: ThreadViewItem) -> usize {
        let idx = self.snapshot.items.len();
        self.snapshot.items.push(item);
        idx
    }

    fn tool_mut(&mut self, call_id: &str) -> Option<&mut ThreadViewItem> {
        let idx = *self.indexes.tools.get(call_id)?;
        self.snapshot.items.get_mut(idx)
    }

    fn finish_thinking(&mut self) {
        self.snapshot.thinking = false;
        if let Some(idx) = self.indexes.latest_thinking.take()
            && let Some(ThreadViewItem::Thinking { turn, complete, .. }) =
                self.snapshot.items.get_mut(idx)
        {
            *complete = true;
            self.indexes.open_thinking.remove(turn);
        }
    }

    fn fail_open_compaction(&mut self, turn: u64) -> Option<usize> {
        self.snapshot.compacting = false;
        let idx = self.indexes.open_compactions.remove(&turn)?;
        self.snapshot.items[idx] = ThreadViewItem::Compaction {
            turn,
            state: ThreadCompactionState::Failed,
        };
        Some(idx)
    }

    fn finish_turn(&mut self, turn: u64, ended: chrono::DateTime<chrono::Utc>) {
        self.snapshot.turn_running = false;
        self.fail_open_compaction(turn);
        self.finish_thinking();
        self.snapshot.pending_questions.clear();
        if let Some(started) = self.snapshot.turn_started_at.get(&turn) {
            let ms = (ended - *started).num_milliseconds().max(0) as u64;
            self.snapshot.turn_duration_ms.insert(turn, ms);
        }
    }

    fn ensure_indexes(&mut self) {
        if self.indexes.ready {
            return;
        }
        self.indexes.ready = true;
        for (idx, item) in self.snapshot.items.iter().enumerate() {
            match item {
                ThreadViewItem::Assistant {
                    turn,
                    complete: false,
                    ..
                } => {
                    self.indexes.open_assistant.insert(*turn, idx);
                }
                ThreadViewItem::Thinking { turn, complete, .. } => {
                    if !complete {
                        self.indexes.open_thinking.insert(*turn, idx);
                    }
                    self.indexes.latest_thinking = Some(idx);
                }
                ThreadViewItem::Compaction {
                    turn,
                    state: ThreadCompactionState::Running,
                } => {
                    self.indexes.open_compactions.insert(*turn, idx);
                }
                ThreadViewItem::ToolCall { call_id, .. } => {
                    self.indexes.tools.insert(call_id.clone(), idx);
                }
                ThreadViewItem::TurnStatus { turn, .. } => {
                    self.indexes.turns.insert(*turn, idx);
                }
                ThreadViewItem::Questions { request_id, .. } => {
                    self.indexes.questions.insert(request_id.clone(), idx);
                }
                ThreadViewItem::User { .. }
                | ThreadViewItem::Assistant { .. }
                | ThreadViewItem::Compaction { .. } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trouve_protocol::Scope;

    fn envelope(cursor: u64, elapsed_ms: i64, event: Event) -> EventEnvelope {
        let start = chrono::DateTime::parse_from_rfc3339("2026-08-05T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        EventEnvelope {
            cursor,
            scope: Scope::Thread("thread".into()),
            ts: start + chrono::Duration::milliseconds(elapsed_ms),
            event,
        }
    }

    fn measured_duration(projection: &ThreadProjection) -> Option<u64> {
        match &projection.snapshot.items[0] {
            ThreadViewItem::ToolCall { duration_ms, .. } => *duration_ms,
            item => panic!("expected tool call, got {item:?}"),
        }
    }

    #[test]
    fn tool_duration_uses_started_and_completed_event_timestamps() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::ToolRequested {
                turn: 1,
                call_id: "command".into(),
                tool: "commandExecution".into(),
                args: serde_json::json!({}),
                requires_approval: true,
            },
        ));
        projection.apply(&envelope(
            2,
            5_000,
            Event::ApprovalResolved {
                call_id: "command".into(),
                decision: ApprovalDecision::Approve,
            },
        ));
        projection.apply(&envelope(
            3,
            6_000,
            Event::ToolStarted {
                call_id: "command".into(),
            },
        ));

        // Exercise the same serialization boundary used by the durable
        // thread-view cache while this command is still running.
        let mut projection: ThreadProjection =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        projection.apply(&envelope(
            4,
            6_050,
            Event::ToolCompleted {
                call_id: "command".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({ "exit_code": 0, "duration_ms": 0 }),
            },
        ));

        assert_eq!(measured_duration(&projection), Some(50));
    }

    #[test]
    fn requested_timestamp_is_fallback_when_started_event_is_absent() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            100,
            Event::ToolRequested {
                turn: 1,
                call_id: "command".into(),
                tool: "commandExecution".into(),
                args: serde_json::json!({}),
                requires_approval: false,
            },
        ));
        projection.apply(&envelope(
            2,
            127,
            Event::ToolCompleted {
                call_id: "command".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({ "exit_code": 0 }),
            },
        ));

        assert_eq!(measured_duration(&projection), Some(27));
    }

    #[test]
    fn approval_timestamp_is_fallback_when_started_event_is_absent() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::ToolRequested {
                turn: 1,
                call_id: "command".into(),
                tool: "commandExecution".into(),
                args: serde_json::json!({}),
                requires_approval: true,
            },
        ));
        projection.apply(&envelope(
            2,
            100,
            Event::ApprovalResolved {
                call_id: "command".into(),
                decision: ApprovalDecision::Approve,
            },
        ));
        projection.apply(&envelope(
            3,
            127,
            Event::ToolCompleted {
                call_id: "command".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({ "exit_code": 0 }),
            },
        ));

        assert_eq!(measured_duration(&projection), Some(27));
    }

    #[test]
    fn compaction_is_one_durable_item_that_completes_in_place() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(1, 0, Event::CompactionStarted { turn: 7 }));

        assert!(projection.snapshot.compacting);
        assert_eq!(
            projection.snapshot.items,
            vec![ThreadViewItem::Compaction {
                turn: 7,
                state: ThreadCompactionState::Running,
            }]
        );

        // Exercise cache deserialization: the open-compaction index must be
        // rebuilt before the completion event updates the existing row.
        let mut projection: ThreadProjection =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        projection.apply(&envelope(
            2,
            250,
            Event::CompactionCompleted {
                turn: 7,
                messages_compacted: 42,
            },
        ));

        assert!(!projection.snapshot.compacting);
        assert_eq!(
            projection.snapshot.items,
            vec![ThreadViewItem::Compaction {
                turn: 7,
                state: ThreadCompactionState::Completed {
                    messages_compacted: 42,
                },
            }]
        );
    }

    #[test]
    fn live_usage_updates_context_without_finishing_the_turn() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::TurnStarted {
                turn: 7,
                mode: "code".into(),
                model: "codex/gpt-5.6-sol".into(),
                thinking_level: Some("max".into()),
            },
        ));
        let usage = trouve_protocol::Usage {
            input_tokens: 10_000,
            output_tokens: 500,
            cached_input_tokens: 80_000,
            context_input_tokens: Some(90_000),
            context_window: Some(258_400),
            ..Default::default()
        };
        projection.apply(&envelope(
            2,
            10,
            Event::TurnUsageUpdated {
                turn: 7,
                usage: usage.clone(),
            },
        ));

        assert!(projection.snapshot.turn_running);
        assert_eq!(projection.snapshot.last_usage, Some(usage));
        assert_eq!(
            projection
                .snapshot
                .turn_thinking_levels
                .get(&7)
                .map(String::as_str),
            Some("max")
        );
        assert!(matches!(
            projection.snapshot.items.first(),
            Some(ThreadViewItem::TurnStatus {
                state: ThreadTurnState::Running,
                ..
            })
        ));
    }

    #[test]
    fn completed_turn_retains_its_checkpoint_id() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::TurnStarted {
                turn: 2,
                mode: "code".into(),
                model: "test/model".into(),
                thinking_level: None,
            },
        ));
        projection.apply(&envelope(
            2,
            20,
            Event::TurnCompleted {
                turn: 2,
                usage: trouve_protocol::Usage::default(),
                checkpoint_id: Some("cp_after_2".into()),
            },
        ));

        assert!(matches!(
            projection.snapshot.items.last(),
            Some(ThreadViewItem::TurnStatus {
                turn: 2,
                state: ThreadTurnState::Completed {
                    checkpoint_id: Some(checkpoint_id),
                    ..
                },
            }) if checkpoint_id == "cp_after_2"
        ));
    }

    #[test]
    fn explicit_thinking_completion_closes_without_followup_output() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::AssistantThinking {
                turn: 4,
                text: "Waiting for the next event.".into(),
            },
        ));
        assert!(projection.snapshot.thinking);

        // Exercise the durable-cache rebuild path: the completion edge must
        // find and close the previously streamed thought after replay too.
        let mut projection: ThreadProjection =
            serde_json::from_str(&serde_json::to_string(&projection).unwrap()).unwrap();
        projection.apply(&envelope(
            2,
            25,
            Event::AssistantThinkingCompleted { turn: 4 },
        ));

        assert!(!projection.snapshot.thinking);
        assert!(matches!(
            projection.snapshot.items.last(),
            Some(ThreadViewItem::Thinking { complete: true, .. })
        ));
    }

    #[test]
    fn normal_turn_output_closes_an_unfinished_compaction() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(1, 0, Event::CompactionStarted { turn: 3 }));
        projection.apply(&envelope(
            2,
            10,
            Event::AssistantThinking {
                turn: 3,
                text: "continuing".into(),
            },
        ));

        assert!(!projection.snapshot.compacting);
        assert!(matches!(
            projection.snapshot.items.first(),
            Some(ThreadViewItem::Compaction {
                turn: 3,
                state: ThreadCompactionState::Failed,
            })
        ));
    }

    #[test]
    fn explicit_compaction_failure_closes_the_running_item() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(1, 0, Event::CompactionStarted { turn: 3 }));
        projection.apply(&envelope(2, 10, Event::CompactionFailed { turn: 3 }));

        assert!(!projection.snapshot.compacting);
        assert!(matches!(
            projection.snapshot.items.first(),
            Some(ThreadViewItem::Compaction {
                turn: 3,
                state: ThreadCompactionState::Failed,
            })
        ));
    }
}
