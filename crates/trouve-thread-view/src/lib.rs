//! Fold durable thread events into a protocol-level current-state snapshot.
//! The event log remains authoritative; this projection is rebuildable.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use trouve_protocol::{
    ApprovalDecision, Event, EventEnvelope, ThreadToolStatus, ThreadTurnState, ThreadViewItem,
    ThreadViewSnapshot, ToolStatus,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ThreadProjection {
    pub cursor: u64,
    pub snapshot: ThreadViewSnapshot,
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
    questions: HashMap<String, usize>,
    latest_thinking: Option<usize>,
}

impl ThreadProjection {
    pub fn apply(&mut self, envelope: &EventEnvelope) {
        self.ensure_indexes();
        self.cursor = envelope.cursor;
        match &envelope.event {
            Event::TurnStarted { turn, model, .. } => {
                self.snapshot.turn_running = true;
                self.snapshot.turn_models.insert(*turn, model.clone());
                self.snapshot.turn_started_at.insert(*turn, envelope.ts);
                let idx = self.push(ThreadViewItem::TurnStatus {
                    turn: *turn,
                    state: ThreadTurnState::Running,
                });
                self.indexes.turns.insert(*turn, idx);
            }
            Event::CompactionStarted { .. } => self.snapshot.compacting = true,
            Event::CommandsUpdated { commands } => self.snapshot.commands = commands.clone(),
            Event::QueueUpdated { prompts } => self.snapshot.queue = prompts.clone(),
            Event::TodosUpdated { todos } => self.snapshot.todos = todos.clone(),
            Event::CompactionCompleted { .. } => self.snapshot.compacting = false,
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
            Event::AssistantDelta { turn, text } => {
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
                call_id,
                tool,
                args,
                requires_approval,
                ..
            } => {
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
                });
                self.indexes.tools.insert(call_id.clone(), idx);
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
            }
            Event::ToolStarted { call_id } => {
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
                    }
                }
            }
            Event::ToolCompleted {
                call_id,
                status,
                result,
            } => {
                if let Some(ThreadViewItem::ToolCall {
                    status: current,
                    result: current_result,
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
                }
                self.snapshot.pending_approvals.retain(|id| id != call_id);
            }
            Event::QuestionRequested {
                request_id,
                title,
                questions,
                ..
            } => {
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
            Event::TurnCompleted { turn, usage, .. } => {
                self.finish_turn(*turn, envelope.ts);
                self.snapshot.last_usage = Some(usage.clone());
                if let Some(&idx) = self.indexes.turns.get(turn) {
                    self.snapshot.items[idx] = ThreadViewItem::TurnStatus {
                        turn: *turn,
                        state: ThreadTurnState::Completed {
                            usage: usage.clone(),
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

    fn finish_turn(&mut self, turn: u64, ended: chrono::DateTime<chrono::Utc>) {
        self.snapshot.turn_running = false;
        self.snapshot.compacting = false;
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
                ThreadViewItem::ToolCall { call_id, .. } => {
                    self.indexes.tools.insert(call_id.clone(), idx);
                }
                ThreadViewItem::TurnStatus { turn, .. } => {
                    self.indexes.turns.insert(*turn, idx);
                }
                ThreadViewItem::Questions { request_id, .. } => {
                    self.indexes.questions.insert(request_id.clone(), idx);
                }
                ThreadViewItem::User { .. } | ThreadViewItem::Assistant { .. } => {}
            }
        }
    }
}
