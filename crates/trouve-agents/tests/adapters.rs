//! Adapter e2e tests against stub vendor binaries (shell scripts emitting
//! canned stream-json / JSON-RPC fixtures), so CI needs no vendor CLIs or
//! accounts.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use trouve_agents::claude::ClaudeBackend;
use trouve_agents::codex::CodexBackend;
use trouve_agents::cursor::CursorBackend;
use trouve_agents::{AgentBackend, BackendEvent, BackendPermission, BackendTurn};

fn write_stub(dir: &Path, name: &str, script: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_string()
}

fn turn(worktree: PathBuf, session: Option<&str>, permission: BackendPermission) -> BackendTurn {
    BackendTurn {
        thread_id: "th_1".into(),
        worktree,
        session: session.map(str::to_string),
        model: "test-model".into(),
        model_options: serde_json::Map::new(),
        prompt: "do the thing".into(),
        attachments: vec![],
        instructions: Some("mode prompt".into()),
        permission,
        mcp_bridge: None,
        mcp_servers: Vec::new(),
    }
}

/// Start a turn, retrying the classic parallel-test ETXTBSY race: a fork
/// in a sibling test can briefly hold this stub's write fd open when we
/// exec it.
async fn start_turn<B: AgentBackend>(
    backend: &B,
    make_turn: impl Fn() -> BackendTurn,
) -> trouve_agents::BackendEventStream {
    for _ in 0..50 {
        match backend.run_turn(make_turn()).await {
            Err(trouve_agents::BackendError::Io(e))
                if e.raw_os_error() == Some(26 /* ETXTBSY */) =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            other => return other.unwrap(),
        }
    }
    panic!("spawn kept hitting ETXTBSY");
}

#[tokio::test]
async fn claude_adapter_maps_stream_json() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "claude",
        r#"#!/bin/bash
printf '%s\n' "$@" > "$0.args"
cat <<'EOF'
{"type":"system","subtype":"init","session_id":"sess-1"}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm."}}}
{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"","estimated_tokens":50}}}
{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello"}}}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"Hmm.","signature":"sig"},{"type":"text","text":"Hello"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"files"}]}}
{"type":"result","subtype":"success","session_id":"sess-2","usage":{"input_tokens":10,"output_tokens":5},"total_cost_usd":0.01}
EOF
"#,
    );
    let backend = ClaudeBackend::new("claude-code", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        turn(
            tmp.path().to_path_buf(),
            Some("old-sess"),
            BackendPermission::ReadOnly,
        )
    })
    .await;

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }

    // Session ids captured from init and result (claude rotates per resume).
    let sessions: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::SessionStarted { session_id } => Some(session_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(sessions, vec!["sess-1", "sess-2"]);

    // Text and thinking come only from the streamed deltas: exactly once
    // each (the complete assistant message must not re-emit them), and the
    // empty redacted thinking delta is dropped.
    let texts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["Hello"]);
    let thinking: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::ThinkingDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, vec!["Hmm."]);
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::ToolStarted { call_id, tool, .. } if call_id == "t1" && tool == "Bash")
    ));
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::ToolCompleted { call_id, ok: true, .. } if call_id == "t1")
    ));
    // Cost stays unset: the CLI's estimate is misleading on subscriptions.
    assert!(events.iter().any(|e| matches!(
        e,
        BackendEvent::Completed { usage } if usage.input_tokens == 10
            && usage.output_tokens == 5
            && usage.cost_usd.is_none()
    )));

    // Flags: resume + read-only permission mapping + mode instructions.
    // Read-only avoids `--permission-mode plan` (its interactive plan
    // workflow prompt misfires headless); mutating built-ins are disallowed
    // and everything else is denied through the approval gate.
    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("--resume"), "{args}");
    assert!(args.contains("old-sess"), "{args}");
    assert!(!args.contains("--permission-mode"), "{args}");
    assert!(args.contains("--disallowedTools"), "{args}");
    assert!(args.contains("Write,Edit,MultiEdit,NotebookEdit"), "{args}");
    assert!(args.contains("--append-system-prompt"), "{args}");
    assert!(args.contains("--model"), "{args}");
    assert!(args.contains("--include-partial-messages"), "{args}");
    assert!(args.contains("--thinking-display"), "{args}");
}

/// ACP stub for cursor-agent: answers the fixed request sequence of a fresh
/// turn (initialize, session/new, set mode, set model, prompt), streams a
/// text delta + tool call, raises one permission request, and records what
/// it received.
fn cursor_acp_stub(dir: &Path) -> String {
    write_stub(
        dir,
        "cursor-agent",
        r##"#!/bin/bash
echo "$1" > "$0.args"
read line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
read line # session/new
echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}'
read line # set_config_option mode
echo "$line" > "$0.mode"
echo '{"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"mode","currentValue":"agent"}]}}'
read line # set_config_option model
echo "$line" > "$0.model"
echo '{"jsonrpc":"2.0","id":4,"result":{"configOptions":[{"id":"model","currentValue":"test-model"}]}}'
read line # session/prompt
echo "$line" > "$0.prompt"
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Hmm."}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hi "}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"there"}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call","toolCallId":"c1","title":"`ls`","kind":"execute","status":"pending","rawInput":{"command":"ls"}}}}'
echo '{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"c1","title":"`ls`","kind":"execute"},"options":[{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"allow-always","name":"Allow always","kind":"allow_always"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]}}'
read approval
echo "$approval" > "$0.approval"
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","rawOutput":{"exitCode":0,"stdout":"a.txt\n"}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call","toolCallId":"c2","title":"Create Plan","kind":"other","status":"pending","rawInput":{"_toolName":"createPlan"}}}}'
echo '{"jsonrpc":"2.0","id":101,"method":"cursor/create_plan","params":{"toolCallId":"c2","name":"Plan","plan":"# The plan"}}'
read planack
echo "$planack" > "$0.planack"
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"c2","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call","toolCallId":"c3","title":"Ask Question","kind":"think","status":"pending","rawInput":{"_toolName":"askQuestion"}}}}'
echo '{"jsonrpc":"2.0","id":102,"method":"cursor/ask_question","params":{"toolCallId":"c3","title":"Prefs","questions":[{"id":"q1","prompt":"Color?","options":[{"id":"red","label":"Red"},{"id":"blue","label":"Blue"}],"allowMultiple":false}]}}'
read qans
echo "$qans" > "$0.qans"
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"c3","status":"completed"}}}'
echo '{"jsonrpc":"2.0","id":5,"result":{"stopReason":"end_turn","usage":{"inputTokens":7,"outputTokens":3,"totalTokens":10}}}'
cat > /dev/null
"##,
    )
}

#[tokio::test]
async fn cursor_adapter_speaks_acp_and_bridges_approvals() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = cursor_acp_stub(tmp.path());
    let backend = CursorBackend::new("cursor", Some(stub.clone()), None);
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let mut events = Vec::new();
    let mut asked = None;
    while let Some(ev) = stream.next().await {
        let ev = ev.unwrap();
        if let BackendEvent::ApprovalNeeded {
            call_id,
            tool,
            responder,
            ..
        } = ev
        {
            assert_eq!(call_id, "c1");
            assert_eq!(tool, "execute");
            responder.send(true).unwrap();
            continue;
        }
        if let BackendEvent::QuestionsNeeded {
            request_id,
            title,
            questions,
            responder,
        } = ev
        {
            asked = Some((request_id, title, questions.clone()));
            responder
                .send(Some(vec![trouve_protocol::QuestionAnswer {
                    question_id: questions[0].id.clone(),
                    selected_option_ids: vec!["red".into()],
                    other_text: Some("crimson, really".into()),
                }]))
                .unwrap();
            continue;
        }
        events.push(ev);
    }

    // Fresh thread: the ACP session id is persisted for resume.
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::SessionStarted { session_id } if session_id == "sess-1")
    ));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hi there");
    let thinking: String = events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::ThinkingDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "Hmm.");
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::ToolStarted { call_id, tool, .. } if call_id == "c1" && tool == "execute")
    ));
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::ToolCompleted { call_id, ok: true, .. } if call_id == "c1")
    ));
    // Plan mode: catch-all "other" calls surface their real tool name, the
    // cursor/create_plan request is acked (else the turn hangs), and its
    // stashed content becomes the plan tool's result.
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::ToolStarted { call_id, tool, .. } if call_id == "c2" && tool == "createPlan")
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        BackendEvent::ToolCompleted { call_id, ok: true, result }
            if call_id == "c2" && result["plan"] == "# The plan"
    )));
    let planack = std::fs::read_to_string(format!("{stub}.planack")).unwrap();
    assert!(planack.contains("\"id\":101"), "{planack}");
    assert!(planack.contains("\"result\":{}"), "{planack}");
    // The session-less cursor/ask_question request routed to the turn via
    // its toolCallId, surfaced as QuestionsNeeded, and our answers went
    // back in cursor's outcome shape.
    let (request_id, title, questions) = asked.expect("QuestionsNeeded surfaced");
    assert_eq!(request_id, "c3");
    assert_eq!(title.as_deref(), Some("Prefs"));
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].prompt, "Color?");
    assert_eq!(questions[0].options[1].label, "Blue");
    assert!(!questions[0].allow_multiple);
    let qans = std::fs::read_to_string(format!("{stub}.qans")).unwrap();
    assert!(qans.contains("\"id\":102"), "{qans}");
    assert!(qans.contains("\"outcome\":\"answered\""), "{qans}");
    assert!(qans.contains("\"selectedOptionIds\":[\"red\"]"), "{qans}");
    assert!(
        qans.contains("\"freeformText\":\"crimson, really\""),
        "{qans}"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        BackendEvent::Completed { usage } if usage.input_tokens == 7 && usage.output_tokens == 3
    )));

    // The child ran in ACP mode and got our config before the prompt.
    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert_eq!(args.trim(), "acp");
    let mode = std::fs::read_to_string(format!("{stub}.mode")).unwrap();
    assert!(mode.contains("\"configId\":\"mode\""), "{mode}");
    assert!(mode.contains("\"value\":\"agent\""), "{mode}");
    let model = std::fs::read_to_string(format!("{stub}.model")).unwrap();
    assert!(model.contains("\"configId\":\"model\""), "{model}");
    assert!(model.contains("\"value\":\"test-model\""), "{model}");
    // Mode instructions ride in the first prompt of a fresh session.
    let prompt = std::fs::read_to_string(format!("{stub}.prompt")).unwrap();
    assert!(prompt.contains("mode-instructions"), "{prompt}");
    assert!(prompt.contains("do the thing"), "{prompt}");

    // Our approval reply picked the allow-once option.
    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("\"id\":100"), "{reply}");
    assert!(reply.contains("allow-once"), "{reply}");
}

#[tokio::test]
async fn cursor_adapter_auto_approves_in_yolo_and_maps_read_only_to_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = cursor_acp_stub(tmp.path());
    let backend = CursorBackend::new("cursor", Some(stub.clone()), None);
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Yolo)
    })
    .await;

    // Yolo: the permission request is answered inside the adapter, so no
    // ApprovalNeeded ever surfaces.
    let mut saw_approval = false;
    let mut completed = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            BackendEvent::ApprovalNeeded { .. } => saw_approval = true,
            BackendEvent::Completed { .. } => completed = true,
            _ => {}
        }
    }
    assert!(!saw_approval);
    assert!(completed);
    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("allow-once"), "{reply}");

    // Read-only turns run in cursor's plan mode (fresh process: the shared
    // ACP child is per-backend, so use a new backend instance).
    let tmp2 = tempfile::tempdir().unwrap();
    let stub2 = cursor_acp_stub(tmp2.path());
    let backend2 = CursorBackend::new("cursor", Some(stub2.clone()), None);
    let mut stream2 = start_turn(&backend2, || {
        turn(tmp2.path().to_path_buf(), None, BackendPermission::ReadOnly)
    })
    .await;
    while let Some(ev) = stream2.next().await {
        if let BackendEvent::ApprovalNeeded { responder, .. } = ev.unwrap() {
            let _ = responder.send(false);
        }
    }
    let mode = std::fs::read_to_string(format!("{stub2}.mode")).unwrap();
    assert!(mode.contains("\"value\":\"plan\""), "{mode}");
}

#[tokio::test]
async fn codex_adapter_speaks_json_rpc_and_bridges_approvals() {
    let tmp = tempfile::tempdir().unwrap();
    // Deterministic request ids (initialize=1, thread/start=2, turn/start=3)
    // let the stub hardcode its responses. It pauses on the approval request
    // and records our decision before finishing the turn.
    let stub = write_stub(
        tmp.path(),
        "codex",
        r#"#!/bin/bash
read line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
read line # initialized notification
read line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
read line # turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","itemId":"i1","delta":"Hello"}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"thr-1","item":{"id":"c1","type":"commandExecution","command":"ls"}}}'
echo '{"jsonrpc":"2.0","id":100,"method":"item/commandExecution/requestApproval","params":{"threadId":"thr-1","itemId":"c1","command":"ls"}}'
read approval
echo "$approval" > "$0.approval"
echo '{"jsonrpc":"2.0","method":"item/commandExecution/outputDelta","params":{"threadId":"thr-1","itemId":"c1","delta":"a.txt\n"}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"threadId":"thr-1","item":{"id":"c1","type":"commandExecution","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","tokenUsage":{"inputTokens":11,"outputTokens":4}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let mut saw_text = false;
    let mut saw_tool_started = false;
    let mut saw_tool_output = false;
    let mut saw_tool_completed = false;
    let mut sessions = Vec::new();
    let mut usage = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            BackendEvent::SessionStarted { session_id } => sessions.push(session_id),
            BackendEvent::TextDelta(t) => saw_text |= t == "Hello",
            BackendEvent::ToolStarted { call_id, .. } => saw_tool_started |= call_id == "c1",
            BackendEvent::ToolOutput { call_id, .. } => saw_tool_output |= call_id == "c1",
            BackendEvent::ToolCompleted { call_id, ok, .. } => {
                saw_tool_completed |= call_id == "c1" && ok
            }
            BackendEvent::ApprovalNeeded {
                call_id,
                tool,
                responder,
                ..
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(tool, "commandExecution");
                responder.send(true).unwrap();
            }
            BackendEvent::Completed { usage: u } => usage = Some(u),
            BackendEvent::ThinkingDelta(_)
            | BackendEvent::QuestionsNeeded { .. }
            | BackendEvent::CommandsUpdated { .. } => {}
        }
    }

    assert_eq!(sessions, vec!["thr-1"]);
    assert!(saw_text && saw_tool_started && saw_tool_output && saw_tool_completed);
    let usage = usage.expect("turn completed");
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 4);

    // Our approval reply reached the vendor with an approved decision.
    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("\"id\":100"), "{reply}");
    assert!(reply.contains("approved"), "{reply}");
}

#[tokio::test]
async fn claude_adapter_wires_mcp_tool_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "claude",
        r#"#!/bin/bash
printf '%s\n' "$@" > "$0.args"
cat <<'EOF'
{"type":"result","subtype":"success","session_id":"s","usage":{"input_tokens":1,"output_tokens":1}}
EOF
"#,
    );
    let backend = ClaudeBackend::new("claude-code", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        let mut t = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
        t.mcp_bridge = Some(trouve_agents::McpBridgeConfig {
            url: "http://127.0.0.1:1/internal/threads/th_1/mcp?tools=1&approval=1".into(),
            bridge_tools: true,
            disallowed_tools: vec!["Bash".into(), "Edit".into(), "Write".into()],
        });
        t.mcp_servers = vec![trouve_agents::McpServerLaunch {
            name: "jira".into(),
            command: "jira-mcp".into(),
            args: vec!["--stdio".into()],
            env: vec![("TOKEN".into(), "sekrit".into())],
        }];
        t
    })
    .await;
    while let Some(ev) = stream.next().await {
        ev.unwrap();
    }

    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("--mcp-config"), "{args}");
    assert!(args.contains("--strict-mcp-config"), "{args}");
    assert!(args.contains("--disallowedTools"), "{args}");
    assert!(args.contains("Bash,Edit,Write"), "{args}");
    assert!(args.contains("--allowedTools"), "{args}");
    assert!(args.contains("mcp__trouve"), "{args}");
    // Ask mode: Claude's permission requests route to the bridge's gate.
    assert!(args.contains("--permission-prompt-tool"), "{args}");
    assert!(args.contains("mcp__trouve__approval_prompt"), "{args}");

    // The generated MCP config points at the engine's embedded HTTP MCP
    // endpoint for this thread.
    let config_path = std::env::temp_dir().join("trouve-mcp-th_1.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["trouve"]["type"], "http");
    assert_eq!(
        config["mcpServers"]["trouve"]["url"],
        "http://127.0.0.1:1/internal/threads/th_1/mcp?tools=1&approval=1"
    );
    assert!(config["mcpServers"]["trouve"]["command"].is_null());
    // User MCP servers ride along in the same config, but are not
    // pre-allowed: their tools go through the normal permission path.
    assert_eq!(config["mcpServers"]["jira"]["command"], "jira-mcp");
    assert_eq!(config["mcpServers"]["jira"]["env"]["TOKEN"], "sekrit");
    assert!(!args.contains("mcp__jira"), "{args}");
    let _ = std::fs::remove_file(config_path);
}

#[tokio::test]
async fn claude_adapter_wires_approval_gate_without_tool_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "claude",
        r#"#!/bin/bash
printf '%s\n' "$@" > "$0.args"
cat <<'EOF'
{"type":"result","subtype":"success","session_id":"s","usage":{"input_tokens":1,"output_tokens":1}}
EOF
"#,
    );
    let backend = ClaudeBackend::new("claude-code", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        let mut t = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
        t.thread_id = "th_2".into();
        t.mcp_bridge = Some(trouve_agents::McpBridgeConfig {
            url: "http://127.0.0.1:1/internal/threads/th_2/mcp?tools=0&approval=1".into(),
            bridge_tools: false,
            disallowed_tools: Vec::new(),
        });
        t
    })
    .await;
    while let Some(ev) = stream.next().await {
        ev.unwrap();
    }

    // Approvals-only: Claude keeps its built-in tools but permission
    // requests route to trouve, and trouve's read-only semantic search
    // tools ride along pre-allowed.
    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("--mcp-config"), "{args}");
    assert!(args.contains("--permission-prompt-tool"), "{args}");
    assert!(args.contains("mcp__trouve__approval_prompt"), "{args}");
    assert!(!args.contains("--disallowedTools"), "{args}");
    assert!(
        args.contains("mcp__trouve__search,mcp__trouve__find_related"),
        "{args}"
    );
    let _ = std::fs::remove_file(std::env::temp_dir().join("trouve-mcp-th_2.json"));
}

#[tokio::test]
async fn claude_adapter_reuses_process_across_turns() {
    let tmp = tempfile::tempdir().unwrap();
    // Persistent stub: one spawn serves many stdin turns, like the real
    // CLI in stream-json input mode.
    let stub = write_stub(
        tmp.path(),
        "claude",
        r#"#!/bin/bash
printf '%s\n' "$@" > "$0.args"
echo spawned >> "$0.spawns"
while IFS= read -r line; do
  echo "$line" >> "$0.stdin"
  echo '{"type":"system","subtype":"init","session_id":"sess-A"}'
  echo '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}}'
  echo '{"type":"result","subtype":"success","session_id":"sess-A","usage":{"input_tokens":1,"output_tokens":1}}'
done
"#,
    );
    let backend = ClaudeBackend::new("claude-code", Some(stub.clone()));

    // First turn: fresh session, spawns the process.
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;
    let mut first = Vec::new();
    while let Some(ev) = stream.next().await {
        first.push(ev.unwrap());
    }
    assert!(
        first
            .iter()
            .any(|e| matches!(e, BackendEvent::Completed { .. }))
    );

    // Second turn resumes the session the process holds: no new spawn.
    let mut stream = start_turn(&backend, || {
        turn(
            tmp.path().to_path_buf(),
            Some("sess-A"),
            BackendPermission::Ask,
        )
    })
    .await;
    let mut second = Vec::new();
    while let Some(ev) = stream.next().await {
        second.push(ev.unwrap());
    }
    assert!(
        second
            .iter()
            .any(|e| matches!(e, BackendEvent::Completed { .. }))
    );

    let spawns = std::fs::read_to_string(format!("{stub}.spawns")).unwrap();
    assert_eq!(spawns.lines().count(), 1, "expected one spawn: {spawns}");
    // Both prompts arrived over the same process's stdin.
    let stdin = std::fs::read_to_string(format!("{stub}.stdin")).unwrap();
    assert_eq!(stdin.lines().count(), 2, "{stdin}");
    assert!(stdin.contains("do the thing"), "{stdin}");
    // Stream-json input mode is on; the prompt is not in argv.
    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("--input-format"), "{args}");
    assert!(!args.contains("do the thing"), "{args}");

    // A turn with a different config (model) forces a respawn.
    let mut stream = start_turn(&backend, || {
        let mut t = turn(
            tmp.path().to_path_buf(),
            Some("sess-A"),
            BackendPermission::Ask,
        );
        t.model = "other-model".into();
        t
    })
    .await;
    while let Some(ev) = stream.next().await {
        ev.unwrap();
    }
    let spawns = std::fs::read_to_string(format!("{stub}.spawns")).unwrap();
    assert_eq!(spawns.lines().count(), 2, "{spawns}");
}

#[tokio::test]
async fn status_reports_missing_binary() {
    let backend = ClaudeBackend::new("claude-code", Some("/nonexistent/claude".into()));
    assert!(!backend.status().installed);
    let backend = CursorBackend::new("cursor", Some("/nonexistent/cursor-agent".into()), None);
    assert!(!backend.status().installed);
    let backend = CodexBackend::new("codex", Some("/nonexistent/codex".into()));
    assert!(!backend.status().installed);
}
