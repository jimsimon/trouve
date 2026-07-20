//! Fold the thread event stream into renderable chat items. UI layers map
//! `ChatItem`s onto their widgets; the folding logic lives here once, and is
//! plain Rust (testable without any UI).

use std::collections::HashMap;

use trouve_protocol::{
    ApprovalDecision, Event, EventEnvelope, Question, QuestionAnswer, ToolStatus, Usage,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ChatItem {
    User {
        turn: u64,
        content: String,
        /// Files uploaded with the prompt (metadata only; bytes are served
        /// at `GET /v1/attachments/{id}`).
        attachments: Vec<trouve_protocol::Attachment>,
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
    ToolCall {
        call_id: String,
        tool: String,
        args: serde_json::Value,
        status: ToolCallStatus,
        result: Option<serde_json::Value>,
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
    Running,
    Completed { usage: Usage },
    Failed { error: String },
}

/// State of one thread's chat, folded from its event stream.
#[derive(Default)]
pub struct ThreadViewModel {
    pub items: Vec<ChatItem>,
    pub cursor: u64,
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
    fn finish_thinking(&mut self) {
        self.thinking = false;
        if let Some(ChatItem::Thinking { complete, .. }) = self
            .items
            .iter_mut()
            .rev()
            .find(|i| matches!(i, ChatItem::Thinking { .. }))
        {
            *complete = true;
        }
    }

    /// Wall-clock time of a finished turn, from its started/ended envelope
    /// timestamps (persisted, so replayed history keeps its durations).
    fn record_turn_duration(&mut self, turn: u64, ended: chrono::DateTime<chrono::Utc>) {
        if let Some(started) = self.turn_started_at.get(&turn) {
            let ms = (ended - *started).num_milliseconds().max(0) as u64;
            self.turn_duration_ms.insert(turn, ms);
        }
    }

    /// Apply one event. Returns the index of the item that changed (for
    /// minimal UI updates), or `None` when nothing visible changed.
    pub fn apply(&mut self, envelope: &EventEnvelope) -> Option<usize> {
        self.cursor = envelope.cursor;
        match &envelope.event {
            Event::TurnStarted { turn, model, .. } => {
                self.turn_running = true;
                self.turn_models.insert(*turn, model.clone());
                self.turn_started_at.insert(*turn, envelope.ts);
                self.items.push(ChatItem::TurnStatus {
                    turn: *turn,
                    state: TurnState::Running,
                });
                Some(self.items.len() - 1)
            }
            Event::CompactionStarted { .. } => {
                self.compacting = true;
                None
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
                self.todos = todos.clone();
                None
            }
            Event::CompactionCompleted { .. } => {
                self.compacting = false;
                None
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
            Event::AssistantThinking { turn, text } => {
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
            Event::AssistantDelta { turn, text } => {
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
                call_id,
                tool,
                args,
                requires_approval,
                ..
            } => {
                self.finish_thinking();
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
                });
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
                idx
            }
            Event::ToolStarted { call_id } => {
                let idx = self.items.iter().rposition(
                    |i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id),
                );
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
                    }
                }
                idx
            }
            Event::ToolCompleted {
                call_id,
                status,
                result,
            } => {
                let idx = self.items.iter().rposition(
                    |i| matches!(i, ChatItem::ToolCall { call_id: c, .. } if c == call_id),
                );
                if let Some(ChatItem::ToolCall {
                    status: s,
                    result: r,
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
                }
                self.pending_approvals.retain(|c| c != call_id);
                idx
            }
            Event::QuestionRequested {
                request_id,
                title,
                questions,
                ..
            } => {
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
            Event::TurnCompleted { turn, usage, .. } => {
                self.turn_running = false;
                self.compacting = false;
                self.finish_thinking();
                self.pending_questions.clear();
                self.last_usage = Some(usage.clone());
                self.record_turn_duration(*turn, envelope.ts);
                let idx = self.items.iter().rposition(|i| {
                    matches!(i, ChatItem::TurnStatus { turn: t, state: TurnState::Running } if t == turn)
                });
                if let Some(idx) = idx {
                    self.items[idx] = ChatItem::TurnStatus {
                        turn: *turn,
                        state: TurnState::Completed {
                            usage: usage.clone(),
                        },
                    };
                }
                idx
            }
            Event::TurnFailed { turn, error } => {
                self.turn_running = false;
                self.compacting = false;
                self.finish_thinking();
                self.pending_questions.clear();
                self.record_turn_duration(*turn, envelope.ts);
                let idx = self.items.iter().rposition(|i| {
                    matches!(i, ChatItem::TurnStatus { turn: t, state: TurnState::Running } if t == turn)
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
                self.turn_running = false;
                self.compacting = false;
                self.finish_thinking();
                self.pending_questions.clear();
                self.record_turn_duration(*turn, envelope.ts);
                let idx = self.items.iter().position(|i| {
                    matches!(i, ChatItem::TurnStatus { turn: t, state: TurnState::Running } if t == turn)
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
    fn turn_duration_computed_from_envelope_timestamps() {
        let mut vm = ThreadViewModel::new();
        let start = chrono_now();
        let mut started = env(Event::TurnStarted {
            turn: 1,
            mode: "code".into(),
            model: "m".into(),
        });
        started.ts = start;
        vm.apply(&started);
        let mut completed = env(Event::TurnCompleted {
            turn: 1,
            usage: Usage::default(),
            checkpoint_id: None,
        });
        completed.ts = start + chrono::Duration::milliseconds(12_400);
        vm.apply(&completed);
        assert_eq!(vm.turn_duration_ms.get(&1), Some(&12_400));
    }

    #[test]
    fn todo_snapshot_replaces_previous_state_without_adding_chat_rows() {
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
    fn full_turn_folds_into_expected_items() {
        let mut vm = ThreadViewModel::new();
        for event in [
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "m".into(),
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
        }));
        assert!(vm.turn_running);
        assert!(!vm.compacting);

        vm.apply(&env(Event::CompactionStarted { turn: 1 }));
        assert!(vm.compacting);
        vm.apply(&env(Event::CompactionCompleted {
            turn: 1,
            messages_compacted: 5,
        }));
        assert!(!vm.compacting);

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
    fn turn_cancelled_clears_running_state() {
        let mut vm = ThreadViewModel::new();
        vm.apply(&env(Event::TurnStarted {
            turn: 1,
            mode: "code".into(),
            model: "m".into(),
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
}
