//! Fold durable thread events into a protocol-level current-state snapshot.
//! The event log remains authoritative; this projection is rebuildable.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use trouve_protocol::{
    ApprovalDecision, Event, EventEnvelope, ThreadCompactionState, ThreadTodoState,
    ThreadToolStatus, ThreadTurnState, ThreadViewItem, ThreadViewSnapshot, TodoItem, TodoStatus,
    ToolStatus,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ThreadProjection {
    pub cursor: u64,
    pub snapshot: ThreadViewSnapshot,
    /// Number of completed folded items already persisted as independently
    /// pageable rows. `snapshot.items` contains only the live/unmaterialized
    /// suffix after this absolute offset.
    #[serde(default)]
    materialized_items: u64,
    /// One hidden turn-boundary bit for each item in the live suffix. A
    /// cancelled turn deliberately has no visible status row, so the boundary
    /// must survive independently for bounded, turn-aligned pagination.
    #[serde(default)]
    turn_starts: Vec<bool>,
    /// Execution anchors are projection state rather than protocol state.
    /// They are serialized with the cache so a command that spans a cache
    /// refresh still receives an accurate duration when it completes.
    #[serde(default)]
    tool_started_at: HashMap<String, chrono::DateTime<chrono::Utc>>,
    /// Normally capacity follows turn.started. Retain an early capacity event
    /// until its shell arrives so replay remains deterministic even when
    /// importing historical streams with the opposite ordering.
    #[serde(default)]
    capacity_acquired_before_start: HashSet<u64>,
    #[serde(skip)]
    indexes: ProjectionIndexes,
}

#[derive(Debug, Default)]
struct ProjectionIndexes {
    ready: bool,
    open_assistant: HashMap<u64, usize>,
    open_progress: HashMap<u64, usize>,
    open_thinking: HashMap<u64, usize>,
    tools: HashMap<String, usize>,
    turns: HashMap<u64, usize>,
    open_compactions: HashMap<u64, usize>,
    questions: HashMap<String, usize>,
    latest_progress: Option<usize>,
    latest_thinking: Option<usize>,
}

#[derive(Debug, PartialEq)]
pub struct MaterializedThreadItem {
    pub item: ThreadViewItem,
    pub turn_start: bool,
}

impl ThreadProjection {
    pub fn apply(&mut self, envelope: &EventEnvelope) {
        self.ensure_indexes();
        self.cursor = envelope.cursor;
        match &envelope.event {
            Event::TurnCapacityAcquired { turn, .. } => {
                if let Some(&idx) = self.indexes.turns.get(turn) {
                    if matches!(
                        self.snapshot.items.get(idx),
                        Some(ThreadViewItem::TurnStatus {
                            state: ThreadTurnState::WaitingForCapacity,
                            ..
                        })
                    ) {
                        self.snapshot.items[idx] = ThreadViewItem::TurnStatus {
                            turn: *turn,
                            state: ThreadTurnState::Running,
                        };
                    }
                } else {
                    self.capacity_acquired_before_start.insert(*turn);
                }
            }
            Event::TurnStarted {
                turn,
                model,
                thinking_level,
                supports_steering,
                ..
            } => {
                self.snapshot.turn_running = true;
                self.snapshot.turn_models.insert(*turn, model.clone());
                if let Some(thinking_level) = thinking_level {
                    self.snapshot
                        .turn_thinking_levels
                        .insert(*turn, thinking_level.clone());
                }
                self.snapshot
                    .turn_steerable
                    .insert(*turn, *supports_steering);
                self.snapshot.turn_started_at.insert(*turn, envelope.ts);
                let state = if self.capacity_acquired_before_start.remove(turn) {
                    ThreadTurnState::Running
                } else {
                    ThreadTurnState::WaitingForCapacity
                };
                let idx = self.push_turn_start(ThreadViewItem::TurnStatus { turn: *turn, state });
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
            Event::TodosUpdated { todos } => {
                let turn = self.active_turn();
                let previous = std::mem::replace(&mut self.snapshot.todos, todos.clone());
                if let Some(turn) = turn {
                    for (todo, state) in todo_transitions(&previous, todos) {
                        self.push(ThreadViewItem::TodoUpdate {
                            turn,
                            todo_id: todo.id.clone(),
                            content: todo.content.clone(),
                            state,
                        });
                    }
                }
            }
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
            Event::TurnSteered {
                turn,
                content,
                attachments,
            } => {
                self.finish_progress();
                self.finish_thinking();
                self.push(ThreadViewItem::Steered {
                    turn: *turn,
                    content: content.clone(),
                    attachments: attachments.clone(),
                });
            }
            Event::SubagentSpawned {
                turn,
                thread_id,
                session_id,
                prompt,
                model,
                call_id,
            } => {
                self.fail_open_compaction(*turn);
                self.finish_progress();
                self.finish_thinking();
                self.push(ThreadViewItem::Subagent {
                    turn: *turn,
                    thread_id: thread_id.clone(),
                    session_id: session_id.clone(),
                    prompt: prompt.clone(),
                    model: model.clone(),
                    call_id: call_id.clone(),
                });
            }
            Event::AssistantProgress { turn, text } => {
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                if let Some(&idx) = self.indexes.open_progress.get(turn) {
                    if let ThreadViewItem::Progress { content, .. } = &mut self.snapshot.items[idx]
                    {
                        content.push_str(text);
                    }
                } else {
                    let idx = self.push(ThreadViewItem::Progress {
                        turn: *turn,
                        content: text.clone(),
                        complete: false,
                    });
                    self.indexes.open_progress.insert(*turn, idx);
                    self.indexes.latest_progress = Some(idx);
                }
            }
            Event::AssistantProgressCompleted { .. } => {
                self.finish_progress();
            }
            Event::AssistantThinking { turn, text } => {
                self.fail_open_compaction(*turn);
                self.finish_progress();
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
                self.finish_progress();
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
                self.finish_progress();
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
                self.finish_progress();
                self.finish_thinking();
                let idx = self.push(ThreadViewItem::ToolCall {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    args: args.clone(),
                    details_deferred: false,
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
                execution_duration_ms,
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
                    if execution_duration_ms.is_some() || measured_duration_ms.is_some() {
                        *duration_ms = execution_duration_ms.or(measured_duration_ms);
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
                self.finish_progress();
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
                    self.turn_starts.remove(idx);
                    // Cancellation hides the status row but not the turn's
                    // transcript. Move its boundary to the first remaining
                    // item so pagination cannot merge it with older turns.
                    if let Some(turn_start) = self.turn_starts.get_mut(idx) {
                        *turn_start = true;
                    }
                    self.indexes = ProjectionIndexes::default();
                }
            }
            _ => {}
        }
    }

    fn push(&mut self, item: ThreadViewItem) -> usize {
        let idx = self.snapshot.items.len();
        self.snapshot.items.push(item);
        self.turn_starts.push(false);
        idx
    }

    fn push_turn_start(&mut self, item: ThreadViewItem) -> usize {
        let idx = self.push(item);
        self.turn_starts[idx] = true;
        idx
    }

    fn active_turn(&self) -> Option<u64> {
        self.snapshot
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                ThreadViewItem::TurnStatus {
                    turn,
                    state: ThreadTurnState::WaitingForCapacity | ThreadTurnState::Running,
                } => Some(*turn),
                _ => None,
            })
    }

    /// Absolute folded-item offset of the current live suffix.
    pub fn materialized_items(&self) -> u64 {
        self.materialized_items
    }

    /// Complete folded item count without loading any materialized row.
    pub fn total_items(&self) -> u64 {
        self.materialized_items + self.snapshot.items.len() as u64
    }

    /// Hidden boundaries corresponding one-for-one with the live item suffix.
    pub fn live_turn_starts(&self) -> &[bool] {
        debug_assert_eq!(self.turn_starts.len(), self.snapshot.items.len());
        &self.turn_starts
    }

    /// Remove the completed prefix that can be persisted independently. An
    /// active turn remains in the live projection because subsequent stream
    /// events can still mutate its thinking, assistant, approval, question,
    /// and tool rows in place.
    pub fn take_materializable_prefix(&mut self) -> (u64, Vec<MaterializedThreadItem>) {
        self.ensure_indexes();
        let keep_from = if self.snapshot.turn_running {
            self.snapshot
                .items
                .iter()
                .rposition(|item| {
                    matches!(
                        item,
                        ThreadViewItem::TurnStatus { state, .. }
                            if matches!(
                                state,
                                ThreadTurnState::WaitingForCapacity | ThreadTurnState::Running
                            )
                    )
                })
                .unwrap_or(0)
        } else {
            self.snapshot.items.len()
        };
        if keep_from == 0 {
            return (self.materialized_items, Vec::new());
        }
        let start = self.materialized_items;
        let items = self.snapshot.items.drain(..keep_from).collect::<Vec<_>>();
        let turn_starts = self.turn_starts.drain(..keep_from).collect::<Vec<_>>();
        let items = items
            .into_iter()
            .zip(turn_starts)
            .map(|(item, turn_start)| MaterializedThreadItem { item, turn_start })
            .collect::<Vec<_>>();
        self.materialized_items += items.len() as u64;
        self.indexes = ProjectionIndexes::default();
        (start, items)
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

    fn finish_progress(&mut self) {
        if let Some(idx) = self.indexes.latest_progress.take()
            && let Some(ThreadViewItem::Progress { turn, complete, .. }) =
                self.snapshot.items.get_mut(idx)
        {
            *complete = true;
            self.indexes.open_progress.remove(turn);
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
        self.capacity_acquired_before_start.remove(&turn);
        self.snapshot.turn_running = false;
        self.fail_open_compaction(turn);
        self.finish_progress();
        self.finish_thinking();
        self.abort_open_tools(ended);
        self.snapshot.pending_questions.clear();
        if let Some(started) = self.snapshot.turn_started_at.get(&turn) {
            let ms = (ended - *started).num_milliseconds().max(0) as u64;
            self.snapshot.turn_duration_ms.insert(turn, ms);
        }
    }

    /// A thread has at most one active turn, so every non-terminal tool row
    /// belongs to the turn that is ending. Provider control-plane calls can
    /// disappear without a matching tool.completed event when the provider
    /// interrupts or closes a turn; never leave those rows looking active in
    /// a replayed transcript.
    fn abort_open_tools(&mut self, ended: chrono::DateTime<chrono::Utc>) {
        for item in &mut self.snapshot.items {
            let ThreadViewItem::ToolCall {
                call_id,
                status,
                duration_ms,
                ..
            } = item
            else {
                continue;
            };
            if !matches!(
                *status,
                ThreadToolStatus::Running | ThreadToolStatus::AwaitingApproval
            ) {
                continue;
            }
            *status = ThreadToolStatus::Aborted;
            if duration_ms.is_none()
                && let Some(started) = self.tool_started_at.remove(call_id)
            {
                *duration_ms = Some((ended - started).num_milliseconds().max(0) as u64);
            } else {
                self.tool_started_at.remove(call_id);
            }
        }
        self.snapshot.pending_approvals.clear();
    }

    fn ensure_indexes(&mut self) {
        if self.indexes.ready {
            return;
        }
        if self.turn_starts.len() != self.snapshot.items.len() {
            self.turn_starts.resize(self.snapshot.items.len(), false);
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
                ThreadViewItem::Progress { turn, complete, .. } => {
                    if !complete {
                        self.indexes.open_progress.insert(*turn, idx);
                    }
                    self.indexes.latest_progress = Some(idx);
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
                    self.turn_starts[idx] = true;
                    self.indexes.turns.insert(*turn, idx);
                }
                ThreadViewItem::Questions { request_id, .. } => {
                    self.indexes.questions.insert(request_id.clone(), idx);
                }
                ThreadViewItem::User { .. }
                | ThreadViewItem::Steered { .. }
                | ThreadViewItem::Subagent { .. }
                | ThreadViewItem::Assistant { .. }
                | ThreadViewItem::TodoUpdate { .. }
                | ThreadViewItem::Compaction { .. } => {}
            }
        }
    }
}

fn todo_transitions<'a>(
    previous: &'a [TodoItem],
    current: &'a [TodoItem],
) -> Vec<(&'a TodoItem, ThreadTodoState)> {
    let previous_by_id = previous
        .iter()
        .map(|todo| (todo.id.as_str(), todo))
        .collect::<HashMap<_, _>>();
    let current_by_id = current
        .iter()
        .map(|todo| (todo.id.as_str(), todo))
        .collect::<HashMap<_, _>>();
    let mut transitions = Vec::new();
    for todo in current {
        let previous_status = previous_by_id.get(todo.id.as_str()).map(|todo| todo.status);
        let state = match todo.status {
            TodoStatus::InProgress if previous_status != Some(TodoStatus::InProgress) => {
                Some(ThreadTodoState::Started)
            }
            TodoStatus::Completed if previous_status != Some(TodoStatus::Completed) => {
                Some(ThreadTodoState::Completed)
            }
            TodoStatus::Cancelled if previous_status != Some(TodoStatus::Cancelled) => {
                Some(ThreadTodoState::Cancelled)
            }
            TodoStatus::Pending
            | TodoStatus::InProgress
            | TodoStatus::Completed
            | TodoStatus::Cancelled => None,
        };
        if let Some(state) = state {
            transitions.push((todo, state));
        }
    }
    for todo in previous {
        if !current_by_id.contains_key(todo.id.as_str())
            && !matches!(todo.status, TodoStatus::Completed | TodoStatus::Cancelled)
        {
            transitions.push((todo, ThreadTodoState::Skipped));
        }
    }
    transitions
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
    fn turn_transitions_from_waiting_to_running_when_capacity_arrives() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::TurnStarted {
                turn: 7,
                mode: "code".into(),
                model: "m".into(),
                thinking_level: None,
                supports_steering: false,
            },
        ));
        assert!(matches!(
            projection.snapshot.items.last(),
            Some(ThreadViewItem::TurnStatus {
                turn: 7,
                state: ThreadTurnState::WaitingForCapacity,
            })
        ));

        projection.apply(&envelope(
            2,
            20,
            Event::TurnCapacityAcquired {
                turn: 7,
                wait_ms: 20,
                background: false,
            },
        ));
        assert!(matches!(
            projection.snapshot.items.last(),
            Some(ThreadViewItem::TurnStatus {
                turn: 7,
                state: ThreadTurnState::Running,
            })
        ));
    }

    #[test]
    fn capacity_before_start_replays_as_running() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::TurnCapacityAcquired {
                turn: 9,
                wait_ms: 0,
                background: false,
            },
        ));
        projection.apply(&envelope(
            2,
            1,
            Event::TurnStarted {
                turn: 9,
                mode: "code".into(),
                model: "m".into(),
                thinking_level: None,
                supports_steering: false,
            },
        ));
        assert!(matches!(
            projection.snapshot.items.last(),
            Some(ThreadViewItem::TurnStatus {
                turn: 9,
                state: ThreadTurnState::Running,
            })
        ));
    }

    #[test]
    fn todo_updates_materialize_lifecycle_rows_during_a_turn() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::TurnStarted {
                turn: 7,
                mode: "code".into(),
                model: "m".into(),
                thinking_level: None,
                supports_steering: false,
            },
        ));
        projection.apply(&envelope(
            2,
            1,
            Event::TodosUpdated {
                todos: vec![
                    TodoItem {
                        id: "one".into(),
                        content: "First".into(),
                        status: TodoStatus::InProgress,
                    },
                    TodoItem {
                        id: "two".into(),
                        content: "Second".into(),
                        status: TodoStatus::Pending,
                    },
                ],
            },
        ));
        projection.apply(&envelope(
            3,
            2,
            Event::TodosUpdated {
                todos: vec![TodoItem {
                    id: "one".into(),
                    content: "First".into(),
                    status: TodoStatus::Completed,
                }],
            },
        ));

        let updates = projection
            .snapshot
            .items
            .iter()
            .filter_map(|item| match item {
                ThreadViewItem::TodoUpdate { todo_id, state, .. } => {
                    Some((todo_id.as_str(), *state))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            updates,
            vec![
                ("one", ThreadTodoState::Started),
                ("one", ThreadTodoState::Completed),
                ("two", ThreadTodoState::Skipped),
            ]
        );
    }

    #[test]
    fn todo_snapshot_outside_a_turn_does_not_add_transcript_rows() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::TodosUpdated {
                todos: vec![TodoItem {
                    id: "one".into(),
                    content: "First".into(),
                    status: TodoStatus::InProgress,
                }],
            },
        ));
        assert!(projection.snapshot.items.is_empty());
        assert_eq!(projection.snapshot.todos.len(), 1);
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
                execution_duration_ms: Some(7),
            },
        ));

        assert_eq!(measured_duration(&projection), Some(7));
    }

    #[test]
    fn terminal_turn_aborts_an_unmatched_running_tool() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "codex/model".into(),
                thinking_level: None,
                supports_steering: false,
            },
        ));
        projection.apply(&envelope(
            2,
            10,
            Event::ToolRequested {
                turn: 1,
                call_id: "wait".into(),
                tool: "collabAgentToolCall".into(),
                args: serde_json::json!({ "tool": "wait" }),
                requires_approval: false,
            },
        ));
        projection.apply(&envelope(
            3,
            20,
            Event::ToolStarted {
                call_id: "wait".into(),
            },
        ));
        projection.apply(&envelope(4, 270, Event::TurnCancelled { turn: 1 }));

        assert!(matches!(
            projection.snapshot.items.first(),
            Some(ThreadViewItem::ToolCall {
                call_id,
                status: ThreadToolStatus::Aborted,
                duration_ms: Some(250),
                ..
            }) if call_id == "wait"
        ));
        assert!(!projection.snapshot.turn_running);
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
                execution_duration_ms: None,
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
                execution_duration_ms: None,
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
    fn subagent_spawn_is_a_durable_parent_turn_boundary() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::AssistantThinking {
                turn: 7,
                text: "Delegating the review.".into(),
            },
        ));
        projection.apply(&envelope(
            2,
            1,
            Event::SubagentSpawned {
                turn: 7,
                thread_id: "th_child".into(),
                session_id: "se_child".into(),
                prompt: "Review the host lifecycle.".into(),
                model: "codex/gpt-5.6-terra".into(),
                call_id: Some("call_spawn".into()),
            },
        ));

        assert_eq!(
            projection.snapshot.items,
            vec![
                ThreadViewItem::Thinking {
                    turn: 7,
                    content: "Delegating the review.".into(),
                    complete: true,
                },
                ThreadViewItem::Subagent {
                    turn: 7,
                    thread_id: "th_child".into(),
                    session_id: "se_child".into(),
                    prompt: "Review the host lifecycle.".into(),
                    model: "codex/gpt-5.6-terra".into(),
                    call_id: Some("call_spawn".into()),
                },
            ]
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
                supports_steering: false,
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
            Event::TurnCapacityAcquired {
                turn: 7,
                wait_ms: 10,
                background: false,
            },
        ));
        projection.apply(&envelope(
            3,
            11,
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
    fn steering_is_a_causal_boundary_between_thinking_items() {
        let mut projection = ThreadProjection::default();
        for (cursor, event) in [
            Event::TurnStarted {
                turn: 7,
                mode: "code".into(),
                model: "codex/gpt-5.6-sol".into(),
                thinking_level: Some("max".into()),
                supports_steering: true,
            },
            Event::UserMessage {
                turn: 7,
                content: "Start here.".into(),
                attachments: Vec::new(),
            },
            Event::AssistantThinking {
                turn: 7,
                text: "Before steering.".into(),
            },
            Event::TurnSteered {
                turn: 7,
                content: "Prioritize the regression.".into(),
                attachments: Vec::new(),
            },
            Event::AssistantThinking {
                turn: 7,
                text: "After steering.".into(),
            },
            Event::AssistantThinkingCompleted { turn: 7 },
        ]
        .into_iter()
        .enumerate()
        {
            projection.apply(&envelope(cursor as u64 + 1, cursor as i64, event));
        }

        assert_eq!(projection.snapshot.turn_steerable.get(&7), Some(&true));
        assert!(projection.snapshot.turn_running);
        assert!(matches!(
            projection.snapshot.items.as_slice(),
            [
                ThreadViewItem::TurnStatus { .. },
                ThreadViewItem::User { content: prompt, .. },
                ThreadViewItem::Thinking {
                    content: before,
                    complete: true,
                    ..
                },
                ThreadViewItem::Steered { content: steering, .. },
                ThreadViewItem::Thinking {
                    content: after,
                    complete: true,
                    ..
                },
            ] if prompt == "Start here."
                && before == "Before steering."
                && steering == "Prioritize the regression."
                && after == "After steering."
        ));
        assert!(!projection.snapshot.thinking);
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
                supports_steering: false,
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
    fn cancelled_turn_moves_hidden_boundary_to_first_visible_item() {
        let mut projection = ThreadProjection::default();
        for (cursor, event) in [
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "test/model".into(),
                thinking_level: None,
                supports_steering: false,
            },
            Event::UserMessage {
                turn: 1,
                content: "keep this prompt".into(),
                attachments: Vec::new(),
            },
            Event::AssistantMessage {
                turn: 1,
                content: "partial response".into(),
            },
            Event::TurnCancelled { turn: 1 },
        ]
        .into_iter()
        .enumerate()
        {
            projection.apply(&envelope(cursor as u64 + 1, cursor as i64, event));
        }

        assert_eq!(projection.live_turn_starts(), &[true, false]);
        assert!(
            !projection
                .snapshot
                .items
                .iter()
                .any(|item| matches!(item, ThreadViewItem::TurnStatus { .. }))
        );
        let (_, materialized) = projection.take_materializable_prefix();
        assert!(materialized[0].turn_start);
        assert!(!materialized[1].turn_start);
    }

    #[test]
    fn materialization_drains_completed_turns_but_keeps_the_live_turn_mutable() {
        let mut projection = ThreadProjection::default();
        for (cursor, event) in [
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "test/model".into(),
                thinking_level: None,
                supports_steering: false,
            },
            Event::UserMessage {
                turn: 1,
                content: "first".into(),
                attachments: Vec::new(),
            },
            Event::TurnCompleted {
                turn: 1,
                usage: Default::default(),
                checkpoint_id: None,
            },
            Event::TurnStarted {
                turn: 2,
                mode: "code".into(),
                model: "test/model".into(),
                thinking_level: None,
                supports_steering: false,
            },
            Event::AssistantThinking {
                turn: 2,
                text: "still ".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            projection.apply(&envelope(cursor as u64 + 1, cursor as i64, event));
        }

        let (start, completed) = projection.take_materializable_prefix();
        assert_eq!(start, 0);
        assert_eq!(completed.len(), 2);
        assert_eq!(projection.materialized_items(), 2);
        assert_eq!(projection.snapshot.items.len(), 2);
        assert!(matches!(
            projection.snapshot.items.first(),
            Some(ThreadViewItem::TurnStatus {
                turn: 2,
                state: ThreadTurnState::WaitingForCapacity,
            })
        ));

        projection.apply(&envelope(
            6,
            6,
            Event::AssistantThinking {
                turn: 2,
                text: "running".into(),
            },
        ));
        assert!(matches!(
            projection.snapshot.items.last(),
            Some(ThreadViewItem::Thinking { content, .. }) if content == "still running"
        ));

        projection.apply(&envelope(
            7,
            7,
            Event::TurnCompleted {
                turn: 2,
                usage: Default::default(),
                checkpoint_id: None,
            },
        ));
        let (next_start, second_turn) = projection.take_materializable_prefix();
        assert_eq!(next_start, 2);
        assert_eq!(second_turn.len(), 2);
        assert!(projection.snapshot.items.is_empty());
        assert_eq!(projection.total_items(), 4);
    }

    #[test]
    fn progress_and_reasoning_remain_distinct_folded_items() {
        let mut projection = ThreadProjection::default();
        for (cursor, event) in [
            Event::AssistantProgress {
                turn: 4,
                text: "Checking the adapter.".into(),
            },
            Event::AssistantProgressCompleted { turn: 4 },
            Event::AssistantThinking {
                turn: 4,
                text: "The provider emits a separate reasoning stream.".into(),
            },
            Event::AssistantThinkingCompleted { turn: 4 },
        ]
        .into_iter()
        .enumerate()
        {
            projection.apply(&envelope(cursor as u64 + 1, cursor as i64, event));
        }

        assert!(matches!(
            projection.snapshot.items.as_slice(),
            [
                ThreadViewItem::Progress {
                    content: progress,
                    complete: true,
                    ..
                },
                ThreadViewItem::Thinking {
                    content: reasoning,
                    complete: true,
                    ..
                },
            ] if progress == "Checking the adapter."
                && reasoning == "The provider emits a separate reasoning stream."
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
    fn tool_request_is_a_causal_boundary_between_thinking_items() {
        let mut projection = ThreadProjection::default();
        projection.apply(&envelope(
            1,
            0,
            Event::AssistantThinking {
                turn: 4,
                text: "The final overlap pass is still".into(),
            },
        ));
        projection.apply(&envelope(
            2,
            10,
            Event::ToolRequested {
                turn: 4,
                call_id: "search".into(),
                tool: "search_transcript".into(),
                args: serde_json::json!({ "query": "Stopping" }),
                requires_approval: false,
            },
        ));
        projection.apply(&envelope(
            3,
            20,
            Event::AssistantThinking {
                turn: 4,
                text: " running.".into(),
            },
        ));
        projection.apply(&envelope(
            4,
            30,
            Event::AssistantThinkingCompleted { turn: 4 },
        ));

        let thoughts = projection
            .snapshot
            .items
            .iter()
            .filter(|item| matches!(item, ThreadViewItem::Thinking { .. }))
            .collect::<Vec<_>>();
        assert_eq!(thoughts.len(), 2);
        assert!(matches!(
            thoughts[0],
            ThreadViewItem::Thinking {
                content,
                complete: true,
                ..
            } if content == "The final overlap pass is still"
        ));
        assert!(matches!(
            thoughts[1],
            ThreadViewItem::Thinking {
                content,
                complete: true,
                ..
            } if content == " running."
        ));
        assert!(!projection.snapshot.thinking);
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
