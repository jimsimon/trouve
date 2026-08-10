//! Fold the thread event stream into renderable chat items. UI layers map
//! `ChatItem`s onto their widgets; the folding logic lives here once, and is
//! plain Rust (testable without any UI).

use std::collections::{HashMap, HashSet};

use trouve_protocol::{
    ApprovalDecision, Event, EventEnvelope, Question, QuestionAnswer, ThreadCompactionState,
    ThreadTodoState, ThreadToolStatus, ThreadTurnState, ThreadViewItem, ThreadViewSnapshot,
    TodoItem, TodoStatus, ToolStatus, Usage,
};

/// Per-tool retained output budget. The projection keeps the latest valid
/// UTF-8 suffix so replaying a long-running command cannot grow client memory
/// without bound.
pub const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolOutputBuffer {
    pub text: String,
    /// True once nonempty earlier output has been discarded.
    pub omitted: bool,
}

impl ToolOutputBuffer {
    fn append(&mut self, chunk: &str) -> bool {
        if chunk.is_empty() {
            return false;
        }

        if chunk.len() >= MAX_TOOL_OUTPUT_BYTES {
            let tail = utf8_tail(chunk, MAX_TOOL_OUTPUT_BYTES);
            self.omitted |= !self.text.is_empty() || tail.len() < chunk.len();
            self.text.clear();
            self.text.push_str(tail);
            return true;
        }

        let retained_budget = MAX_TOOL_OUTPUT_BYTES - chunk.len();
        if self.text.len() > retained_budget {
            self.text = utf8_tail(&self.text, retained_budget).to_owned();
            self.omitted = true;
        }
        self.text.push_str(chunk);
        true
    }
}

/// Return the longest suffix that starts at a UTF-8 scalar boundary and fits
/// within `max_bytes`.
fn utf8_tail(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    if max_bytes == 0 {
        return "";
    }

    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
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

#[derive(Debug, Clone, PartialEq)]
pub enum ChatItem {
    User {
        turn: u64,
        content: String,
        /// Files uploaded with the prompt (metadata only; bytes are served
        /// at `GET /v1/attachments/{id}`).
        attachments: Vec<trouve_protocol::Attachment>,
    },
    /// Additional user guidance appended to an already-running turn.
    Steered {
        turn: u64,
        content: String,
        attachments: Vec<trouve_protocol::Attachment>,
    },
    /// A child-agent transcript linked from its parent turn.
    Subagent {
        turn: u64,
        thread_id: String,
        session_id: String,
        prompt: String,
        model: String,
        call_id: Option<String>,
    },
    /// Streaming or final assistant text (grows in place from deltas).
    Assistant {
        turn: u64,
        content: String,
        complete: bool,
    },
    /// Model reasoning ("thinking") text; closed when other output arrives.
    Thinking {
        turn: u64,
        content: String,
        complete: bool,
    },
    /// Engine context compaction is a durable transcript boundary, not a
    /// provider tool call. Clients render it at the Agent-card top level.
    Compaction {
        turn: u64,
        state: CompactionState,
    },
    /// A durable todo lifecycle transition rendered on the turn rail.
    TodoUpdate {
        turn: u64,
        todo_id: String,
        content: String,
        state: ThreadTodoState,
    },
    ToolCall {
        call_id: String,
        tool: String,
        args: serde_json::Value,
        status: ToolCallStatus,
        result: Option<serde_json::Value>,
        duration_ms: Option<u64>,
    },
    TurnStatus {
        turn: u64,
        state: TurnState,
    },
    /// The agent asked the user questions; while `answers` is `None` the
    /// turn is blocked and clients render the answer wizard.
    Questions {
        request_id: String,
        title: Option<String>,
        questions: Vec<Question>,
        /// Populated by `question.resolved` (inner `None` = skipped).
        answers: Option<Option<Vec<QuestionAnswer>>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    /// Waiting for the user; render approval buttons.
    AwaitingApproval,
    Running,
    Ok,
    Error,
    Denied,
    Aborted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnState {
    WaitingForCapacity,
    Running,
    Completed {
        usage: Usage,
        checkpoint_id: Option<String>,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionState {
    Running,
    Completed { messages_compacted: u64 },
    Failed,
}

/// State of one thread's chat, folded from its event stream.
#[derive(Default)]
pub struct ThreadViewModel {
    pub items: Vec<ChatItem>,
    pub cursor: u64,
    /// Bounded live output for each tool card, keyed by call id. This stays
    /// outside `ChatItem` so existing native renderers remain source-compatible
    /// while web and native projections share replay semantics.
    pub tool_outputs: HashMap<String, ToolOutputBuffer>,
    /// Execution start anchors used to derive the same per-tool durations as
    /// the server projection. Completed snapshot rows carry the result.
    #[doc(hidden)]
    pub tool_started_at: HashMap<String, chrono::DateTime<chrono::Utc>>,
    /// Handles imported/replayed streams where capacity precedes the durable
    /// turn shell. Ordinary live streams transition the visible row directly.
    #[doc(hidden)]
    pub capacity_acquired_before_start: HashSet<u64>,
    /// Call ids currently waiting for approval (newest last).
    pub pending_approvals: Vec<String>,
    /// Question request ids currently waiting for answers (newest last).
    pub pending_questions: Vec<String>,
    /// Usage of the most recently completed turn; its input token count is
    /// the best available proxy for current context size.
    pub last_usage: Option<Usage>,
    /// True between compaction start/complete events (UI busy indicator).
    pub compacting: bool,
    /// True while a turn is running (between turn.started and completion).
    pub turn_running: bool,
    /// True while the model is streaming thinking and nothing has followed
    /// it yet (the "Thinking…" activity label takes priority over tools).
    pub thinking: bool,
    /// The model that ran each turn ("cursor/claude-fable-5"), from
    /// turn.started — shown in the agent card header.
    pub turn_models: HashMap<u64, String>,
    /// The effective provider-native thinking selection for each turn, from
    /// turn.started — shown alongside the model in the agent card header.
    pub turn_thinking_levels: HashMap<u64, String>,
    /// Whether each turn's backend accepted native in-flight steering.
    pub turn_steerable: HashMap<u64, bool>,
    /// When each turn started (the turn.started envelope timestamp);
    /// paired with the completion envelope to compute wall-clock duration.
    pub turn_started_at: HashMap<u64, chrono::DateTime<chrono::Utc>>,
    /// How long each finished turn took, in milliseconds — shown in the
    /// agent card header next to the token summary.
    pub turn_duration_ms: HashMap<u64, u64>,
    /// Slash commands / skills the vendor harness accepts in prompts
    /// (latest announcement wins) — prompt-box completions.
    pub commands: Vec<trouve_protocol::CommandInfo>,
    /// Prompts waiting their turn, in run order (latest announcement wins).
    pub queue: Vec<trouve_protocol::QueuedPrompt>,
    /// Current thread todo snapshot (latest announcement wins).
    pub todos: Vec<trouve_protocol::TodoItem>,
}

impl From<ThreadViewSnapshot> for ThreadViewModel {
    fn from(snapshot: ThreadViewSnapshot) -> Self {
        Self {
            items: snapshot.items.into_iter().map(ChatItem::from).collect(),
            cursor: 0,
            tool_outputs: HashMap::new(),
            tool_started_at: HashMap::new(),
            capacity_acquired_before_start: HashSet::new(),
            pending_approvals: snapshot.pending_approvals,
            pending_questions: snapshot.pending_questions,
            last_usage: snapshot.last_usage,
            compacting: snapshot.compacting,
            turn_running: snapshot.turn_running,
            thinking: snapshot.thinking,
            turn_models: snapshot.turn_models.into_iter().collect(),
            turn_thinking_levels: snapshot.turn_thinking_levels.into_iter().collect(),
            turn_steerable: snapshot.turn_steerable.into_iter().collect(),
            turn_started_at: snapshot.turn_started_at.into_iter().collect(),
            turn_duration_ms: snapshot.turn_duration_ms.into_iter().collect(),
            commands: snapshot.commands,
            queue: snapshot.queue,
            todos: snapshot.todos,
        }
    }
}

impl From<ThreadViewItem> for ChatItem {
    fn from(item: ThreadViewItem) -> Self {
        match item {
            ThreadViewItem::User {
                turn,
                content,
                attachments,
            } => Self::User {
                turn,
                content,
                attachments,
            },
            ThreadViewItem::Steered {
                turn,
                content,
                attachments,
            } => Self::Steered {
                turn,
                content,
                attachments,
            },
            ThreadViewItem::Subagent {
                turn,
                thread_id,
                session_id,
                prompt,
                model,
                call_id,
            } => Self::Subagent {
                turn,
                thread_id,
                session_id,
                prompt,
                model,
                call_id,
            },
            ThreadViewItem::Assistant {
                turn,
                content,
                complete,
            } => Self::Assistant {
                turn,
                content,
                complete,
            },
            ThreadViewItem::Thinking {
                turn,
                content,
                complete,
            } => Self::Thinking {
                turn,
                content,
                complete,
            },
            ThreadViewItem::Compaction { turn, state } => Self::Compaction {
                turn,
                state: match state {
                    ThreadCompactionState::Running => CompactionState::Running,
                    ThreadCompactionState::Completed { messages_compacted } => {
                        CompactionState::Completed { messages_compacted }
                    }
                    ThreadCompactionState::Failed => CompactionState::Failed,
                },
            },
            ThreadViewItem::TodoUpdate {
                turn,
                todo_id,
                content,
                state,
            } => Self::TodoUpdate {
                turn,
                todo_id,
                content,
                state,
            },
            ThreadViewItem::ToolCall {
                call_id,
                tool,
                args,
                details_deferred: _,
                status,
                result,
                duration_ms,
            } => Self::ToolCall {
                call_id,
                tool,
                args,
                status: match status {
                    ThreadToolStatus::AwaitingApproval => ToolCallStatus::AwaitingApproval,
                    ThreadToolStatus::Running => ToolCallStatus::Running,
                    ThreadToolStatus::Ok => ToolCallStatus::Ok,
                    ThreadToolStatus::Error => ToolCallStatus::Error,
                    ThreadToolStatus::Denied => ToolCallStatus::Denied,
                    ThreadToolStatus::Aborted => ToolCallStatus::Aborted,
                },
                result,
                duration_ms,
            },
            ThreadViewItem::TurnStatus { turn, state } => Self::TurnStatus {
                turn,
                state: match state {
                    ThreadTurnState::WaitingForCapacity => TurnState::WaitingForCapacity,
                    ThreadTurnState::Running => TurnState::Running,
                    ThreadTurnState::Completed {
                        usage,
                        checkpoint_id,
                    } => TurnState::Completed {
                        usage,
                        checkpoint_id,
                    },
                    ThreadTurnState::Failed { error } => TurnState::Failed { error },
                },
            },
            ThreadViewItem::Questions {
                request_id,
                title,
                questions,
                resolved,
                answers,
            } => Self::Questions {
                request_id,
                title,
                questions,
                answers: resolved.then_some(answers),
            },
        }
    }
}

impl ThreadViewModel {
    pub fn new() -> Self {
        Self::default()
    }

    fn find_tool(&mut self, call_id: &str) -> Option<&mut ChatItem> {
        self.items
            .iter_mut()
            .rev()
            .find(|i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id))
    }

    /// Close the trailing open thinking block (any non-thinking output ends
    /// it; a later thinking delta starts a fresh block).
    fn finish_thinking(&mut self) -> Option<usize> {
        self.thinking = false;
        let idx = self.items.iter().rposition(|item| {
            matches!(
                item,
                ChatItem::Thinking {
                    complete: false,
                    ..
                }
            )
        })?;
        if let ChatItem::Thinking { complete, .. } = &mut self.items[idx] {
            *complete = true;
        }
        Some(idx)
    }

    fn fail_open_compaction(&mut self, turn: u64) -> Option<usize> {
        self.compacting = false;
        let idx = self.items.iter().rposition(|item| {
            matches!(
                item,
                ChatItem::Compaction {
                    turn: candidate,
                    state: CompactionState::Running,
                } if *candidate == turn
            )
        })?;
        self.items[idx] = ChatItem::Compaction {
            turn,
            state: CompactionState::Failed,
        };
        Some(idx)
    }

    /// Wall-clock time of a finished turn, from its started/ended envelope
    /// timestamps (persisted, so replayed history keeps its durations).
    fn record_turn_duration(&mut self, turn: u64, ended: chrono::DateTime<chrono::Utc>) {
        if let Some(started) = self.turn_started_at.get(&turn) {
            let ms = (ended - *started).num_milliseconds().max(0) as u64;
            self.turn_duration_ms.insert(turn, ms);
        }
    }

    fn active_turn(&self) -> Option<u64> {
        self.items.iter().rev().find_map(|item| match item {
            ChatItem::TurnStatus {
                turn,
                state: TurnState::WaitingForCapacity | TurnState::Running,
            } => Some(*turn),
            _ => None,
        })
    }

    /// Apply one event. Returns the index of the item that changed (for
    /// minimal UI updates), or `None` when nothing visible changed.
    pub fn apply(&mut self, envelope: &EventEnvelope) -> Option<usize> {
        self.cursor = envelope.cursor;
        match &envelope.event {
            Event::TurnCapacityAcquired { turn, .. } => {
                if let Some(idx) = self.items.iter().rposition(|item| {
                    matches!(
                        item,
                        ChatItem::TurnStatus {
                            turn: candidate,
                            state: TurnState::WaitingForCapacity,
                        } if candidate == turn
                    )
                }) {
                    self.items[idx] = ChatItem::TurnStatus {
                        turn: *turn,
                        state: TurnState::Running,
                    };
                    Some(idx)
                } else {
                    self.capacity_acquired_before_start.insert(*turn);
                    None
                }
            }
            Event::TurnStarted {
                turn,
                model,
                thinking_level,
                supports_steering,
                ..
            } => {
                self.turn_running = true;
                self.turn_models.insert(*turn, model.clone());
                if let Some(thinking_level) = thinking_level {
                    self.turn_thinking_levels
                        .insert(*turn, thinking_level.clone());
                }
                self.turn_steerable.insert(*turn, *supports_steering);
                self.turn_started_at.insert(*turn, envelope.ts);
                let state = if self.capacity_acquired_before_start.remove(turn) {
                    TurnState::Running
                } else {
                    TurnState::WaitingForCapacity
                };
                self.items.push(ChatItem::TurnStatus { turn: *turn, state });
                Some(self.items.len() - 1)
            }
            Event::CompactionStarted { turn } => {
                self.compacting = true;
                self.items.push(ChatItem::Compaction {
                    turn: *turn,
                    state: CompactionState::Running,
                });
                Some(self.items.len() - 1)
            }
            Event::CommandsUpdated { commands } => {
                self.commands = commands.clone();
                None
            }
            Event::QueueUpdated { prompts } => {
                self.queue = prompts.clone();
                None
            }
            Event::TodosUpdated { todos } => {
                let turn = self.active_turn();
                let previous = std::mem::replace(&mut self.todos, todos.clone());
                let turn = turn?;
                let transitions = todo_transitions(&previous, todos);
                if transitions.is_empty() {
                    return None;
                }
                for (todo, state) in transitions {
                    self.items.push(ChatItem::TodoUpdate {
                        turn,
                        todo_id: todo.id.clone(),
                        content: todo.content.clone(),
                        state,
                    });
                }
                Some(self.items.len() - 1)
            }
            Event::CompactionCompleted {
                turn,
                messages_compacted,
            } => {
                self.compacting = false;
                let idx = self.items.iter().rposition(|item| {
                    matches!(
                        item,
                        ChatItem::Compaction {
                            turn: candidate,
                            state: CompactionState::Running,
                        } if candidate == turn
                    )
                });
                if let Some(idx) = idx {
                    self.items[idx] = ChatItem::Compaction {
                        turn: *turn,
                        state: CompactionState::Completed {
                            messages_compacted: *messages_compacted,
                        },
                    };
                    Some(idx)
                } else {
                    self.items.push(ChatItem::Compaction {
                        turn: *turn,
                        state: CompactionState::Completed {
                            messages_compacted: *messages_compacted,
                        },
                    });
                    Some(self.items.len() - 1)
                }
            }
            Event::CompactionFailed { turn } => {
                if let Some(idx) = self.fail_open_compaction(*turn) {
                    Some(idx)
                } else {
                    self.compacting = false;
                    self.items.push(ChatItem::Compaction {
                        turn: *turn,
                        state: CompactionState::Failed,
                    });
                    Some(self.items.len() - 1)
                }
            }
            Event::UserMessage {
                turn,
                content,
                attachments,
            } => {
                self.items.push(ChatItem::User {
                    turn: *turn,
                    content: content.clone(),
                    attachments: attachments.clone(),
                });
                Some(self.items.len() - 1)
            }
            Event::TurnSteered {
                turn,
                content,
                attachments,
            } => {
                self.finish_thinking();
                self.items.push(ChatItem::Steered {
                    turn: *turn,
                    content: content.clone(),
                    attachments: attachments.clone(),
                });
                Some(self.items.len() - 1)
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
                self.finish_thinking();
                self.items.push(ChatItem::Subagent {
                    turn: *turn,
                    thread_id: thread_id.clone(),
                    session_id: session_id.clone(),
                    prompt: prompt.clone(),
                    model: model.clone(),
                    call_id: call_id.clone(),
                });
                Some(self.items.len() - 1)
            }
            Event::AssistantThinking { turn, text } => {
                self.fail_open_compaction(*turn);
                self.thinking = true;
                // Grow the trailing open thinking item, or start one.
                if let Some(idx) = self.items.iter().rposition(|i| {
                    matches!(i, ChatItem::Thinking { turn: t, complete: false, .. } if t == turn)
                }) {
                    if let ChatItem::Thinking { content, .. } = &mut self.items[idx] {
                        content.push_str(text);
                    }
                    Some(idx)
                } else {
                    self.items.push(ChatItem::Thinking {
                        turn: *turn,
                        content: text.clone(),
                        complete: false,
                    });
                    Some(self.items.len() - 1)
                }
            }
            Event::AssistantThinkingCompleted { .. } => self.finish_thinking(),
            Event::AssistantDelta { turn, text } => {
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                // Grow the trailing incomplete assistant item, or start one.
                if let Some(idx) = self.items.iter().rposition(|i| {
                    matches!(i, ChatItem::Assistant { turn: t, complete: false, .. } if t == turn)
                }) {
                    if let ChatItem::Assistant { content, .. } = &mut self.items[idx] {
                        content.push_str(text);
                    }
                    Some(idx)
                } else {
                    self.items.push(ChatItem::Assistant {
                        turn: *turn,
                        content: text.clone(),
                        complete: false,
                    });
                    Some(self.items.len() - 1)
                }
            }
            Event::AssistantMessage { turn, content } => {
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                if let Some(idx) = self.items.iter().rposition(|i| {
                    matches!(i, ChatItem::Assistant { turn: t, complete: false, .. } if t == turn)
                }) {
                    self.items[idx] = ChatItem::Assistant {
                        turn: *turn,
                        content: content.clone(),
                        complete: true,
                    };
                    Some(idx)
                } else {
                    self.items.push(ChatItem::Assistant {
                        turn: *turn,
                        content: content.clone(),
                        complete: true,
                    });
                    Some(self.items.len() - 1)
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
                // Call ids are expected to be unique, but resetting here makes
                // a reused id deterministic instead of inheriting stale output.
                self.tool_outputs.remove(call_id);
                self.items.push(ChatItem::ToolCall {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    args: args.clone(),
                    status: if *requires_approval {
                        ToolCallStatus::AwaitingApproval
                    } else {
                        ToolCallStatus::Running
                    },
                    result: None,
                    duration_ms: None,
                });
                if !requires_approval {
                    self.tool_started_at.insert(call_id.clone(), envelope.ts);
                }
                Some(self.items.len() - 1)
            }
            Event::ApprovalRequested { call_id, .. } => {
                if !self.pending_approvals.contains(call_id) {
                    self.pending_approvals.push(call_id.clone());
                }
                // Bridged approvals attach to the vendor's own tool card,
                // which arrived as a plain Running call; flip it so the
                // Approve/Deny UI shows there.
                if let Some(ChatItem::ToolCall { status, .. }) = self.find_tool(call_id) {
                    *status = ToolCallStatus::AwaitingApproval;
                }
                self.items.iter().rposition(
                    |i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id),
                )
            }
            Event::ApprovalResolved { call_id, decision } => {
                self.pending_approvals.retain(|c| c != call_id);
                let denied = *decision == ApprovalDecision::Deny;
                let idx = self.items.iter().rposition(
                    |i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id),
                );
                if let Some(ChatItem::ToolCall { status, .. }) = self.find_tool(call_id) {
                    *status = if denied {
                        ToolCallStatus::Denied
                    } else {
                        ToolCallStatus::Running
                    };
                }
                if !denied {
                    self.tool_started_at
                        .entry(call_id.clone())
                        .or_insert(envelope.ts);
                }
                idx
            }
            Event::ToolStarted { call_id } => {
                let idx = self.items.iter().rposition(
                    |i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id),
                );
                let mut started = false;
                if let Some(ChatItem::ToolCall { status, .. }) = self.find_tool(call_id) {
                    let terminal = matches!(
                        *status,
                        ToolCallStatus::Ok
                            | ToolCallStatus::Error
                            | ToolCallStatus::Denied
                            | ToolCallStatus::Aborted
                    );
                    if !terminal && *status != ToolCallStatus::AwaitingApproval {
                        *status = ToolCallStatus::Running;
                        started = true;
                    }
                }
                if started {
                    self.tool_started_at.insert(call_id.clone(), envelope.ts);
                }
                idx
            }
            Event::ToolOutput { call_id, chunk } => {
                let idx = self.items.iter().rposition(
                    |i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id),
                );
                let terminal = idx.is_some_and(|idx| {
                    matches!(
                        &self.items[idx],
                        ChatItem::ToolCall {
                            status: ToolCallStatus::Ok
                                | ToolCallStatus::Error
                                | ToolCallStatus::Denied
                                | ToolCallStatus::Aborted,
                            ..
                        }
                    )
                });
                if idx.is_none() || terminal || chunk.is_empty() {
                    return None;
                }
                self.tool_outputs
                    .entry(call_id.clone())
                    .or_default()
                    .append(chunk);
                idx
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
                let idx = self.items.iter().rposition(
                    |i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id),
                );
                if let Some(ChatItem::ToolCall {
                    status: s,
                    result: r,
                    duration_ms,
                    ..
                }) = self.find_tool(call_id)
                {
                    // A denied call stays denied: the vendor follows up
                    // with an error tool_result ("user denied"), which
                    // shouldn't repaint the card as a tool failure.
                    if *s != ToolCallStatus::Denied {
                        *s = match status {
                            ToolStatus::Ok => ToolCallStatus::Ok,
                            ToolStatus::Error => ToolCallStatus::Error,
                            ToolStatus::Denied => ToolCallStatus::Denied,
                            ToolStatus::Aborted => ToolCallStatus::Aborted,
                        };
                    }
                    *r = Some(result.clone());
                    if execution_duration_ms.is_some() || measured_duration_ms.is_some() {
                        *duration_ms = execution_duration_ms.or(measured_duration_ms);
                    }
                }
                self.pending_approvals.retain(|c| c != call_id);
                idx
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
                if !self.pending_questions.contains(request_id) {
                    self.pending_questions.push(request_id.clone());
                }
                self.items.push(ChatItem::Questions {
                    request_id: request_id.clone(),
                    title: title.clone(),
                    questions: questions.clone(),
                    answers: None,
                });
                Some(self.items.len() - 1)
            }
            Event::QuestionResolved {
                request_id,
                answers,
            } => {
                self.pending_questions.retain(|r| r != request_id);
                let idx = self.items.iter().rposition(
                    |i| matches!(i, ChatItem::Questions { request_id: r, .. } if r == request_id),
                );
                if let Some(idx) = idx
                    && let ChatItem::Questions { answers: a, .. } = &mut self.items[idx]
                {
                    *a = Some(answers.clone());
                }
                idx
            }
            Event::TurnUsageUpdated { usage, .. } => {
                self.last_usage = Some(usage.clone());
                None
            }
            Event::TurnCompleted {
                turn,
                usage,
                checkpoint_id,
            } => {
                self.capacity_acquired_before_start.remove(turn);
                self.turn_running = false;
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                self.pending_questions.clear();
                self.last_usage = Some(usage.clone());
                self.record_turn_duration(*turn, envelope.ts);
                let idx = self.items.iter().rposition(|i| {
                    matches!(
                        i,
                        ChatItem::TurnStatus {
                            turn: t,
                            state: TurnState::WaitingForCapacity | TurnState::Running,
                        } if t == turn
                    )
                });
                if let Some(idx) = idx {
                    self.items[idx] = ChatItem::TurnStatus {
                        turn: *turn,
                        state: TurnState::Completed {
                            usage: usage.clone(),
                            checkpoint_id: checkpoint_id.clone(),
                        },
                    };
                }
                idx
            }
            Event::TurnFailed { turn, error } => {
                self.capacity_acquired_before_start.remove(turn);
                self.turn_running = false;
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                self.pending_questions.clear();
                self.record_turn_duration(*turn, envelope.ts);
                let idx = self.items.iter().rposition(|i| {
                    matches!(
                        i,
                        ChatItem::TurnStatus {
                            turn: t,
                            state: TurnState::WaitingForCapacity | TurnState::Running,
                        } if t == turn
                    )
                });
                if let Some(idx) = idx {
                    self.items[idx] = ChatItem::TurnStatus {
                        turn: *turn,
                        state: TurnState::Failed {
                            error: error.clone(),
                        },
                    };
                }
                idx
            }
            Event::TurnCancelled { turn } => {
                self.capacity_acquired_before_start.remove(turn);
                self.turn_running = false;
                self.fail_open_compaction(*turn);
                self.finish_thinking();
                self.pending_questions.clear();
                self.record_turn_duration(*turn, envelope.ts);
                let idx = self.items.iter().position(|i| {
                    matches!(
                        i,
                        ChatItem::TurnStatus {
                            turn: t,
                            state: TurnState::WaitingForCapacity | TurnState::Running,
                        } if t == turn
                    )
                });
                if let Some(idx) = idx {
                    self.items.remove(idx);
                }
                idx
            }
            // Session/server scope events don't render in the chat stream.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trouve_protocol::Scope;

    #[test]
    fn shared_web_projection_fixture_matches() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/thread-turn.json")).unwrap();
        let events: Vec<EventEnvelope> = serde_json::from_value(fixture["events"].clone()).unwrap();
        let mut vm = ThreadViewModel::new();
        for event in &events {
            vm.apply(event);
        }

        let item_kinds = vm
            .items
            .iter()
            .map(|item| match item {
                ChatItem::User { .. } => "user",
                ChatItem::Steered { .. } => "steered",
                ChatItem::Subagent { .. } => "subagent",
                ChatItem::Assistant { .. } => "assistant",
                ChatItem::Thinking { .. } => "thinking",
                ChatItem::Compaction { .. } => "compaction",
                ChatItem::TodoUpdate { .. } => "todo",
                ChatItem::ToolCall { .. } => "tool",
                ChatItem::TurnStatus { .. } => "turn-status",
                ChatItem::Questions { .. } => "questions",
            })
            .collect::<Vec<_>>();
        let turn_state = vm.items.iter().find_map(|item| match item {
            ChatItem::TurnStatus { state, .. } => Some(match state {
                TurnState::WaitingForCapacity => "waiting-for-capacity",
                TurnState::Running => "running",
                TurnState::Completed { .. } => "completed",
                TurnState::Failed { .. } => "failed",
            }),
            _ => None,
        });
        let assistant_text = vm.items.iter().find_map(|item| match item {
            ChatItem::Assistant { content, .. } => Some(content.as_str()),
            _ => None,
        });
        let (thinking_text, thinking_complete) = vm
            .items
            .iter()
            .find_map(|item| match item {
                ChatItem::Thinking {
                    content, complete, ..
                } => Some((content.as_str(), *complete)),
                _ => None,
            })
            .unwrap();
        let (tool_call_id, tool_status, tool_result) = vm
            .items
            .iter()
            .find_map(|item| match item {
                ChatItem::ToolCall {
                    call_id,
                    status,
                    result,
                    ..
                } => Some((
                    call_id.as_str(),
                    match status {
                        ToolCallStatus::AwaitingApproval => "awaiting-approval",
                        ToolCallStatus::Running => "running",
                        ToolCallStatus::Ok => "ok",
                        ToolCallStatus::Error => "error",
                        ToolCallStatus::Denied => "denied",
                        ToolCallStatus::Aborted => "aborted",
                    },
                    result.clone(),
                )),
                _ => None,
            })
            .unwrap();
        let tool_output = vm.tool_outputs.get(tool_call_id).unwrap();
        let question_resolution = vm
            .items
            .iter()
            .find_map(|item| match item {
                ChatItem::Questions { answers, .. } => Some(match answers {
                    None => "pending",
                    Some(None) => "skipped",
                    Some(Some(_)) => "answered",
                }),
                _ => None,
            })
            .unwrap();
        let usage = vm.last_usage.as_ref().unwrap();
        let actual = serde_json::json!({
            "cursor": vm.cursor,
            "turn_running": vm.turn_running,
            "thinking": vm.thinking,
            "pending_approvals": vm.pending_approvals,
            "pending_questions": vm.pending_questions,
            "item_kinds": item_kinds,
            "turn_state": turn_state,
            "assistant_text": assistant_text,
            "thinking_text": thinking_text,
            "thinking_complete": thinking_complete,
            "tool_status": tool_status,
            "tool_result": tool_result,
            "tool_output": tool_output.text,
            "tool_output_omitted": tool_output.omitted,
            "question_resolution": question_resolution,
            "command_names": vm.commands.iter().map(|command| command.name.as_str()).collect::<Vec<_>>(),
            "todo_statuses": vm.todos.iter().map(|todo| match todo.status {
                trouve_protocol::TodoStatus::Pending => "pending",
                trouve_protocol::TodoStatus::InProgress => "in_progress",
                trouve_protocol::TodoStatus::Completed => "completed",
                trouve_protocol::TodoStatus::Cancelled => "cancelled",
            }).collect::<Vec<_>>(),
            "turn_duration_ms": vm.turn_duration_ms.get(&7),
            "last_usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cached_input_tokens": usage.cached_input_tokens,
                "cost_usd": usage.cost_usd,
                "context_window": usage.context_window,
            },
        });
        assert_eq!(actual, fixture["expected"]);
    }

    fn env(event: Event) -> EventEnvelope {
        static CURSOR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        EventEnvelope {
            cursor: CURSOR.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            scope: Scope::Thread("th".into()),
            ts: chrono_now(),
            event,
        }
    }

    fn chrono_now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    #[test]
    fn turn_waits_until_capacity_is_acquired() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnStarted {
            turn: 1,
            mode: "code".into(),
            model: "m".into(),
            thinking_level: None,
            supports_steering: false,
        }));
        assert!(matches!(
            vm.items.last(),
            Some(ChatItem::TurnStatus {
                turn: 1,
                state: TurnState::WaitingForCapacity,
            })
        ));

        vm.apply(&env(Event::TurnCapacityAcquired {
            turn: 1,
            wait_ms: 42,
            background: false,
        }));
        assert!(matches!(
            vm.items.last(),
            Some(ChatItem::TurnStatus {
                turn: 1,
                state: TurnState::Running,
            })
        ));
    }

    #[test]
    fn capacity_before_turn_start_replays_as_running() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnCapacityAcquired {
            turn: 3,
            wait_ms: 0,
            background: false,
        }));
        vm.apply(&env(Event::TurnStarted {
            turn: 3,
            mode: "code".into(),
            model: "m".into(),
            thinking_level: None,
            supports_steering: false,
        }));
        assert!(matches!(
            vm.items.last(),
            Some(ChatItem::TurnStatus {
                turn: 3,
                state: TurnState::Running,
            })
        ));
    }

    #[test]
    fn turn_duration_computed_from_envelope_timestamps() {
        let mut vm = ThreadViewModel::new();
        let start = chrono_now();
        let mut started = env(Event::TurnStarted {
            turn: 1,
            mode: "code".into(),
            model: "m".into(),
            thinking_level: None,
            supports_steering: false,
        });
        started.ts = start;
        vm.apply(&started);
        let mut completed = env(Event::TurnCompleted {
            turn: 1,
            usage: Usage::default(),
            checkpoint_id: Some("cp_after_1".into()),
        });
        completed.ts = start + chrono::Duration::milliseconds(12_400);
        vm.apply(&completed);
        assert_eq!(vm.turn_duration_ms.get(&1), Some(&12_400));
        assert!(matches!(
            vm.items.last(),
            Some(ChatItem::TurnStatus {
                turn: 1,
                state: TurnState::Completed {
                    checkpoint_id: Some(checkpoint_id),
                    ..
                },
            }) if checkpoint_id == "cp_after_1"
        ));
    }

    #[test]
    fn todo_snapshot_outside_a_turn_replaces_state_without_chat_rows() {
        let mut vm = ThreadViewModel::new();
        let first = trouve_protocol::TodoItem {
            id: "one".into(),
            content: "First".into(),
            status: trouve_protocol::TodoStatus::InProgress,
        };
        assert_eq!(
            vm.apply(&env(Event::TodosUpdated { todos: vec![first] })),
            None
        );
        let completed = trouve_protocol::TodoItem {
            id: "one".into(),
            content: "First".into(),
            status: trouve_protocol::TodoStatus::Completed,
        };
        vm.apply(&env(Event::TodosUpdated {
            todos: vec![completed.clone()],
        }));

        assert_eq!(vm.todos, vec![completed]);
        assert!(vm.items.is_empty());
    }

    #[test]
    fn todo_updates_add_lifecycle_rows_during_a_turn() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnStarted {
            turn: 7,
            mode: "code".into(),
            model: "m".into(),
            thinking_level: None,
            supports_steering: false,
        }));
        let started = trouve_protocol::TodoItem {
            id: "one".into(),
            content: "First".into(),
            status: trouve_protocol::TodoStatus::InProgress,
        };
        let pending = trouve_protocol::TodoItem {
            id: "two".into(),
            content: "Second".into(),
            status: trouve_protocol::TodoStatus::Pending,
        };
        vm.apply(&env(Event::TodosUpdated {
            todos: vec![started.clone(), pending],
        }));
        vm.apply(&env(Event::TodosUpdated {
            todos: vec![trouve_protocol::TodoItem {
                status: trouve_protocol::TodoStatus::Completed,
                ..started
            }],
        }));

        let updates = vm
            .items
            .iter()
            .filter_map(|item| match item {
                ChatItem::TodoUpdate { todo_id, state, .. } => Some((todo_id.as_str(), *state)),
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
    fn full_turn_folds_into_expected_items() {
        let mut vm = ThreadViewModel::new();
        for event in [
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "m".into(),
                thinking_level: None,
                supports_steering: false,
            },
            Event::UserMessage {
                turn: 1,
                content: "do it".into(),
                attachments: vec![],
            },
            Event::AssistantDelta {
                turn: 1,
                text: "Work".into(),
            },
            Event::AssistantDelta {
                turn: 1,
                text: "ing.".into(),
            },
            Event::AssistantMessage {
                turn: 1,
                content: "Working.".into(),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "c1".into(),
                tool: "write_file".into(),
                args: serde_json::json!({"path": "x"}),
                requires_approval: true,
            },
            Event::ApprovalRequested {
                turn: 1,
                call_id: "c1".into(),
            },
        ] {
            vm.apply(&env(event));
        }
        assert_eq!(vm.pending_approvals, vec!["c1".to_string()]);
        assert!(matches!(
            vm.items.last().unwrap(),
            ChatItem::ToolCall {
                status: ToolCallStatus::AwaitingApproval,
                ..
            }
        ));

        for event in [
            Event::ApprovalResolved {
                call_id: "c1".into(),
                decision: ApprovalDecision::Approve,
            },
            Event::ToolStarted {
                call_id: "c1".into(),
            },
            Event::ToolCompleted {
                call_id: "c1".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({"bytes_written": 3}),
                execution_duration_ms: None,
            },
            Event::TurnCompleted {
                turn: 1,
                usage: Usage::default(),
                checkpoint_id: None,
            },
        ] {
            vm.apply(&env(event));
        }
        assert!(vm.pending_approvals.is_empty());
        assert!(matches!(
            &vm.items[3],
            ChatItem::ToolCall {
                status: ToolCallStatus::Ok,
                result: Some(_),
                ..
            }
        ));
        // Streaming deltas folded into one complete assistant item.
        let assistants: Vec<_> = vm
            .items
            .iter()
            .filter(|i| matches!(i, ChatItem::Assistant { .. }))
            .collect();
        assert_eq!(assistants.len(), 1);
        assert!(matches!(
            assistants[0],
            ChatItem::Assistant { content, complete: true, .. } if content == "Working."
        ));
        assert!(matches!(
            &vm.items[0],
            ChatItem::TurnStatus {
                state: TurnState::Completed { .. },
                ..
            }
        ));
    }

    #[test]
    fn usage_and_compaction_state_track_events() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnStarted {
            turn: 1,
            mode: "code".into(),
            model: "m".into(),
            thinking_level: None,
            supports_steering: false,
        }));
        assert!(vm.turn_running);
        assert!(!vm.compacting);

        let live_usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            cached_input_tokens: 80,
            context_input_tokens: Some(90),
            ..Default::default()
        };
        vm.apply(&env(Event::TurnUsageUpdated {
            turn: 1,
            usage: live_usage.clone(),
        }));
        assert!(vm.turn_running);
        assert_eq!(vm.last_usage, Some(live_usage));

        vm.apply(&env(Event::CompactionStarted { turn: 1 }));
        assert!(vm.compacting);
        vm.apply(&env(Event::CompactionCompleted {
            turn: 1,
            messages_compacted: 5,
        }));
        assert!(!vm.compacting);
        assert!(matches!(
            vm.items.last(),
            Some(ChatItem::Compaction {
                turn: 1,
                state: CompactionState::Completed {
                    messages_compacted: 5,
                },
            })
        ));

        let usage = Usage {
            input_tokens: 1234,
            output_tokens: 56,
            ..Default::default()
        };
        vm.apply(&env(Event::TurnCompleted {
            turn: 1,
            usage: usage.clone(),
            checkpoint_id: None,
        }));
        assert!(!vm.turn_running);
        assert_eq!(vm.last_usage, Some(usage));
    }

    #[test]
    fn approval_before_vendor_tool_card_surfaces_buttons() {
        let mut vm = ThreadViewModel::new();
        // When the engine synthesizes a card before approval.requested…
        vm.apply(&env(Event::ToolRequested {
            turn: 1,
            call_id: "web_search_0".into(),
            tool: "execute".into(),
            args: serde_json::json!({"title": "Web Search"}),
            requires_approval: true,
        }));
        vm.apply(&env(Event::ApprovalRequested {
            turn: 1,
            call_id: "web_search_0".into(),
        }));
        // …a delayed vendor tool_started reuses the card (no duplicate).
        vm.apply(&env(Event::ToolStarted {
            call_id: "web_search_0".into(),
        }));
        assert_eq!(vm.items.len(), 1);
        assert!(matches!(
            &vm.items[0],
            ChatItem::ToolCall {
                status: ToolCallStatus::AwaitingApproval,
                ..
            }
        ));
    }

    #[test]
    fn delayed_tool_started_preserves_denied_cards() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::ToolRequested {
            turn: 1,
            call_id: "c1".into(),
            tool: "Bash".into(),
            args: serde_json::json!({"command": "rm -rf /"}),
            requires_approval: true,
        }));
        vm.apply(&env(Event::ApprovalRequested {
            turn: 1,
            call_id: "c1".into(),
        }));
        vm.apply(&env(Event::ApprovalResolved {
            call_id: "c1".into(),
            decision: ApprovalDecision::Deny,
        }));
        vm.apply(&env(Event::ToolStarted {
            call_id: "c1".into(),
        }));
        assert!(matches!(
            &vm.items[0],
            ChatItem::ToolCall {
                status: ToolCallStatus::Denied,
                ..
            }
        ));
    }

    #[test]
    fn tool_output_folds_into_a_bounded_utf8_tail() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::ToolRequested {
            turn: 1,
            call_id: "c1".into(),
            tool: "Bash".into(),
            args: serde_json::json!({"command": "cargo test"}),
            requires_approval: false,
        }));
        assert_eq!(
            vm.apply(&env(Event::ToolOutput {
                call_id: "c1".into(),
                chunk: "running ".into(),
            })),
            Some(0)
        );
        vm.apply(&env(Event::ToolOutput {
            call_id: "c1".into(),
            chunk: "tests\n".into(),
        }));
        assert_eq!(
            vm.tool_outputs.get("c1"),
            Some(&ToolOutputBuffer {
                text: "running tests\n".into(),
                omitted: false,
            })
        );

        let oversized = format!("{}🙂tail", "x".repeat(MAX_TOOL_OUTPUT_BYTES));
        vm.apply(&env(Event::ToolOutput {
            call_id: "c1".into(),
            chunk: oversized,
        }));
        let retained = vm.tool_outputs.get("c1").unwrap();
        assert!(retained.omitted);
        assert!(retained.text.len() <= MAX_TOOL_OUTPUT_BYTES);
        assert!(retained.text.ends_with("🙂tail"));
    }

    #[test]
    fn incremental_tool_output_trim_preserves_a_utf8_boundary() {
        let mut buffer = ToolOutputBuffer {
            text: format!("{}🙂", "x".repeat(MAX_TOOL_OUTPUT_BYTES - 4)),
            omitted: false,
        };

        assert!(buffer.append("tail"));
        assert!(buffer.omitted);
        assert!(buffer.text.len() <= MAX_TOOL_OUTPUT_BYTES);
        assert!(buffer.text.is_char_boundary(0));
        assert!(buffer.text.ends_with("🙂tail"));
    }

    #[test]
    fn unknown_and_post_completion_tool_output_are_ignored() {
        let mut vm = ThreadViewModel::new();
        assert_eq!(
            vm.apply(&env(Event::ToolOutput {
                call_id: "missing".into(),
                chunk: "ignored".into(),
            })),
            None
        );
        assert!(vm.tool_outputs.is_empty());

        vm.apply(&env(Event::ToolRequested {
            turn: 1,
            call_id: "done".into(),
            tool: "Bash".into(),
            args: serde_json::json!({"command": "true"}),
            requires_approval: false,
        }));
        vm.apply(&env(Event::ToolCompleted {
            call_id: "done".into(),
            status: ToolStatus::Ok,
            result: serde_json::Value::Null,
            execution_duration_ms: None,
        }));
        assert_eq!(
            vm.apply(&env(Event::ToolOutput {
                call_id: "done".into(),
                chunk: "too late".into(),
            })),
            None
        );
        assert!(!vm.tool_outputs.contains_key("done"));
    }

    #[test]
    fn turn_cancelled_clears_running_state() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnStarted {
            turn: 1,
            mode: "code".into(),
            model: "m".into(),
            thinking_level: None,
            supports_steering: false,
        }));
        assert!(vm.turn_running);
        vm.apply(&env(Event::TurnCancelled { turn: 1 }));
        assert!(!vm.turn_running);
        assert!(!vm.items.iter().any(|i| matches!(
            i,
            ChatItem::TurnStatus {
                state: TurnState::Running,
                ..
            }
        )));
    }

    #[test]
    fn bridged_approval_attaches_to_the_vendors_tool_card() {
        let mut vm = ThreadViewModel::new();
        // The vendor's stream announces the call first (plain Running)…
        vm.apply(&env(Event::ToolRequested {
            turn: 1,
            call_id: "toolu_1".into(),
            tool: "Bash".into(),
            args: serde_json::json!({"command": "ls"}),
            requires_approval: false,
        }));
        vm.apply(&env(Event::ToolStarted {
            call_id: "toolu_1".into(),
        }));
        // …then the bridged permission request lands on the same card.
        vm.apply(&env(Event::ApprovalRequested {
            turn: 1,
            call_id: "toolu_1".into(),
        }));
        assert_eq!(vm.items.len(), 1, "no duplicate card for the approval");
        assert!(matches!(
            &vm.items[0],
            ChatItem::ToolCall {
                status: ToolCallStatus::AwaitingApproval,
                ..
            }
        ));
        // Denial sticks even after the vendor's error tool_result.
        vm.apply(&env(Event::ApprovalResolved {
            call_id: "toolu_1".into(),
            decision: ApprovalDecision::Deny,
        }));
        vm.apply(&env(Event::ToolCompleted {
            call_id: "toolu_1".into(),
            status: ToolStatus::Error,
            result: serde_json::json!("user denied"),
            execution_duration_ms: None,
        }));
        assert!(matches!(
            &vm.items[0],
            ChatItem::ToolCall {
                status: ToolCallStatus::Denied,
                result: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn thinking_folds_and_closes_on_other_output() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnStarted {
            turn: 1,
            mode: "code".into(),
            model: "m".into(),
            thinking_level: None,
            supports_steering: false,
        }));
        vm.apply(&env(Event::AssistantThinking {
            turn: 1,
            text: "Let me ".into(),
        }));
        vm.apply(&env(Event::AssistantThinking {
            turn: 1,
            text: "look.".into(),
        }));
        assert!(vm.thinking);
        assert!(matches!(
            vm.items.last().unwrap(),
            ChatItem::Thinking { content, complete: false, .. } if content == "Let me look."
        ));

        // Regular text closes the thinking block and clears the flag.
        vm.apply(&env(Event::AssistantDelta {
            turn: 1,
            text: "Found it.".into(),
        }));
        assert!(!vm.thinking);
        assert!(matches!(
            &vm.items[1],
            ChatItem::Thinking { complete: true, .. }
        ));

        // A later thinking delta starts a fresh block.
        vm.apply(&env(Event::AssistantThinking {
            turn: 1,
            text: "More thought.".into(),
        }));
        let thinking_blocks = vm
            .items
            .iter()
            .filter(|i| matches!(i, ChatItem::Thinking { .. }))
            .count();
        assert_eq!(thinking_blocks, 2);
    }

    #[test]
    fn steering_closes_thinking_and_preserves_capability_and_content() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnStarted {
            turn: 4,
            mode: "code".into(),
            model: "codex/gpt-5.6-sol".into(),
            thinking_level: Some("max".into()),
            supports_steering: true,
        }));
        vm.apply(&env(Event::AssistantThinking {
            turn: 4,
            text: "Original direction.".into(),
        }));
        vm.apply(&env(Event::TurnSteered {
            turn: 4,
            content: "Check the smaller-screen layout too.".into(),
            attachments: Vec::new(),
        }));

        assert_eq!(vm.turn_steerable.get(&4), Some(&true));
        assert!(!vm.thinking);
        assert!(matches!(
            vm.items.as_slice(),
            [
                ChatItem::TurnStatus { .. },
                ChatItem::Thinking { complete: true, .. },
                ChatItem::Steered { turn: 4, content, attachments },
            ] if content == "Check the smaller-screen layout too." && attachments.is_empty()
        ));
    }

    #[test]
    fn explicit_thinking_completion_clears_the_live_phase() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::AssistantThinking {
            turn: 1,
            text: "Waiting.".into(),
        }));
        assert!(vm.thinking);

        let changed = vm.apply(&env(Event::AssistantThinkingCompleted { turn: 1 }));
        assert_eq!(changed, Some(0));
        assert!(!vm.thinking);
        assert!(matches!(
            vm.items.first(),
            Some(ChatItem::Thinking { complete: true, .. })
        ));
    }

    #[test]
    fn questions_fold_into_a_wizard_item_and_resolve() {
        let mut vm = ThreadViewModel::new();
        let questions = vec![Question {
            id: "q1".into(),
            prompt: "Favorite color?".into(),
            options: vec![
                trouve_protocol::QuestionOption {
                    id: "red".into(),
                    label: "Red".into(),
                },
                trouve_protocol::QuestionOption {
                    id: "blue".into(),
                    label: "Blue".into(),
                },
            ],
            allow_multiple: false,
        }];
        vm.apply(&env(Event::QuestionRequested {
            turn: 1,
            request_id: "qr_1".into(),
            title: Some("Quick check".into()),
            questions: questions.clone(),
        }));
        assert_eq!(vm.pending_questions, vec!["qr_1".to_string()]);
        assert!(matches!(
            vm.items.last().unwrap(),
            ChatItem::Questions { answers: None, .. }
        ));

        let answers = vec![QuestionAnswer {
            question_id: "q1".into(),
            selected_option_ids: vec!["red".into()],
            other_text: None,
        }];
        vm.apply(&env(Event::QuestionResolved {
            request_id: "qr_1".into(),
            answers: Some(answers.clone()),
        }));
        assert!(vm.pending_questions.is_empty());
        assert!(matches!(
            vm.items.last().unwrap(),
            ChatItem::Questions { answers: Some(Some(a)), .. } if *a == answers
        ));

        // A skipped request resolves with inner None.
        vm.apply(&env(Event::QuestionRequested {
            turn: 1,
            request_id: "qr_2".into(),
            title: None,
            questions,
        }));
        vm.apply(&env(Event::QuestionResolved {
            request_id: "qr_2".into(),
            answers: None,
        }));
        assert!(matches!(
            vm.items.last().unwrap(),
            ChatItem::Questions {
                answers: Some(None),
                ..
            }
        ));
    }

    #[test]
    fn replay_equals_live() {
        // Applying the same event list twice into two view models gives the
        // same items — the folding is deterministic (replay guarantee).
        let events = vec![
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "m".into(),
                thinking_level: None,
                supports_steering: false,
            },
            Event::UserMessage {
                turn: 1,
                content: "hi".into(),
                attachments: vec![],
            },
            Event::AssistantDelta {
                turn: 1,
                text: "a".into(),
            },
            Event::AssistantMessage {
                turn: 1,
                content: "a".into(),
            },
            Event::TurnCompleted {
                turn: 1,
                usage: Usage::default(),
                checkpoint_id: None,
            },
        ];
        let mut a = ThreadViewModel::new();
        let mut b = ThreadViewModel::new();
        for e in &events {
            a.apply(&env(e.clone()));
        }
        for e in &events {
            b.apply(&env(e.clone()));
        }
        assert_eq!(a.items, b.items);
    }

    #[test]
    fn server_projection_matches_client_event_fold() {
        let events = vec![
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "test/model".into(),
                thinking_level: Some("max".into()),
                supports_steering: false,
            },
            Event::UserMessage {
                turn: 1,
                content: "go".into(),
                attachments: vec![],
            },
            Event::AssistantThinking {
                turn: 1,
                text: "hmm".into(),
            },
            Event::AssistantDelta {
                turn: 1,
                text: "done".into(),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "call_1".into(),
                tool: "read_file".into(),
                args: serde_json::json!({"path": "a.txt"}),
                requires_approval: false,
            },
            Event::ToolStarted {
                call_id: "call_1".into(),
            },
            Event::ToolCompleted {
                call_id: "call_1".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({"content": "a"}),
                execution_duration_ms: None,
            },
            Event::AssistantMessage {
                turn: 1,
                content: "done".into(),
            },
            Event::TurnCompleted {
                turn: 1,
                usage: Usage::default(),
                checkpoint_id: None,
            },
        ];
        let mut client = ThreadViewModel::new();
        let mut server = trouve_thread_view::ThreadProjection::default();
        for event in events {
            let envelope = env(event);
            client.apply(&envelope);
            server.apply(&envelope);
        }
        let mut projected = ThreadViewModel::from(server.snapshot);
        projected.cursor = server.cursor;

        assert_eq!(projected.items, client.items);
        assert_eq!(projected.cursor, client.cursor);
        assert_eq!(projected.pending_approvals, client.pending_approvals);
        assert_eq!(projected.pending_questions, client.pending_questions);
        assert_eq!(projected.last_usage, client.last_usage);
        assert_eq!(projected.turn_running, client.turn_running);
        assert_eq!(projected.thinking, client.thinking);
        assert_eq!(projected.turn_models, client.turn_models);
        assert_eq!(projected.turn_thinking_levels, client.turn_thinking_levels);
        assert_eq!(
            client.turn_thinking_levels.get(&1).map(String::as_str),
            Some("max")
        );
        assert_eq!(projected.turn_duration_ms, client.turn_duration_ms);
    }
}
