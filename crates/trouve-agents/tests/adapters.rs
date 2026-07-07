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
        instructions: Some("mode prompt".into()),
        permission,
        mcp_bridge: None,
    }
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
    let mut stream = backend
        .run_turn(turn(
            tmp.path().to_path_buf(),
            Some("old-sess"),
            BackendPermission::ReadOnly,
        ))
        .await
        .unwrap();

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

#[tokio::test]
async fn cursor_adapter_creates_chat_and_maps_events() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "cursor-agent",
        r#"#!/bin/bash
if [ "$1" = "create-chat" ]; then echo "chat-123"; exit 0; fi
printf '%s\n' "$@" > "$0.args"
cat <<'EOF'
{"type":"system","subtype":"init","session_id":"chat-123"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Hi "}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"there"}]}}
{"type":"tool_call","subtype":"started","call_id":"c1","tool_call":{"readToolCall":{"args":{"path":"a.txt"}}}}
{"type":"tool_call","subtype":"completed","call_id":"c1","tool_call":{"readToolCall":{"args":{"path":"a.txt"},"result":{"content":"x"}}}}
{"type":"result","result":"done","usage":{"input_tokens":7,"output_tokens":3}}
EOF
"#,
    );
    let backend = CursorBackend::new("cursor", Some(stub.clone()), None);
    let mut stream = backend
        .run_turn(turn(
            tmp.path().to_path_buf(),
            None,
            BackendPermission::Yolo,
        ))
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }

    // Fresh thread: create-chat ran and its id is persisted for resume.
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::SessionStarted { session_id } if session_id == "chat-123")
    ));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hi there");
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::ToolStarted { call_id, tool, .. } if call_id == "c1" && tool == "read")
    ));
    assert!(events.iter().any(
        |e| matches!(e, BackendEvent::ToolCompleted { call_id, ok: true, .. } if call_id == "c1")
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        BackendEvent::Completed { usage } if usage.input_tokens == 7 && usage.output_tokens == 3
    )));

    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("--resume"), "{args}");
    assert!(args.contains("chat-123"), "{args}");
    assert!(args.contains("--force"), "{args}"); // yolo mapping
    assert!(args.contains("stream-json"), "{args}");
    // Headless runs abort on the workspace-trust prompt without this.
    assert!(args.contains("--trust"), "{args}");
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
    let mut stream = backend
        .run_turn(turn(tmp.path().to_path_buf(), None, BackendPermission::Ask))
        .await
        .unwrap();

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
            BackendEvent::ThinkingDelta(_) => {}
        }
    }

    assert_eq!(sessions, vec!["thr-1"]);
    assert!(saw_text && saw_tool_started && saw_tool_output && saw_tool_completed);
    let usage = usage.expect("turn completed");
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 4);

    // Our approval reply reached the vendor with an accept decision.
    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("\"id\":100"), "{reply}");
    assert!(reply.contains("accept"), "{reply}");
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
    let mut t = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
    t.mcp_bridge = Some(trouve_agents::McpBridgeConfig {
        command: "trouve".into(),
        args: vec!["mcp-bridge".into()],
        env: vec![
            ("TROUVE_SERVER".into(), "http://127.0.0.1:1".into()),
            ("TROUVE_THREAD_ID".into(), "th_1".into()),
        ],
        bridge_tools: true,
        disallowed_tools: vec!["Bash".into(), "Edit".into(), "Write".into()],
    });
    let mut stream = backend.run_turn(t).await.unwrap();
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

    // The generated MCP config launches the bridge with the dial-back env.
    let config_path = std::env::temp_dir().join("trouve-mcp-th_1.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["trouve"]["command"], "trouve");
    assert_eq!(
        config["mcpServers"]["trouve"]["env"]["TROUVE_THREAD_ID"],
        "th_1"
    );
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
    let mut t = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
    t.thread_id = "th_2".into();
    t.mcp_bridge = Some(trouve_agents::McpBridgeConfig {
        command: "trouve".into(),
        args: vec!["mcp-bridge".into()],
        env: vec![
            ("TROUVE_SERVER".into(), "http://127.0.0.1:1".into()),
            ("TROUVE_THREAD_ID".into(), "th_2".into()),
        ],
        bridge_tools: false,
        disallowed_tools: Vec::new(),
    });
    let mut stream = backend.run_turn(t).await.unwrap();
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
async fn status_reports_missing_binary() {
    let backend = ClaudeBackend::new("claude-code", Some("/nonexistent/claude".into()));
    assert!(!backend.status().installed);
    let backend = CursorBackend::new("cursor", Some("/nonexistent/cursor-agent".into()), None);
    assert!(!backend.status().installed);
    let backend = CodexBackend::new("codex", Some("/nonexistent/codex".into()));
    assert!(!backend.status().installed);
}
