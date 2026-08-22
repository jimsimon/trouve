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
use trouve_agents::{
    AgentBackend, BackendCollaboratorEvent, BackendEvent, BackendPermission, BackendTurn,
};

fn write_stub(dir: &Path, name: &str, script: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_string()
}

fn turn(worktree: PathBuf, session: Option<&str>, permission: BackendPermission) -> BackendTurn {
    BackendTurn {
        cancel: Default::default(),
        thread_id: "th_1".into(),
        worktree,
        session: session.map(str::to_string),
        model: "test-model".into(),
        model_options: serde_json::Map::new(),
        prompt: "do the thing".into(),
        attachments: vec![],
        instructions: Some("mode prompt".into()),
        permission,
        tool_free: false,
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

#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Option<char> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| stat.rsplit_once(") ").map(|(_, tail)| tail.to_owned()))
        .and_then(|tail| tail.chars().next())
}

#[cfg(target_os = "linux")]
async fn wait_for_process_to_stop(pid: u32) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let state = process_state(pid);
            if state.is_none() || state == Some('Z') {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("descendant process {pid} survived Cursor recycle"));
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

#[tokio::test]
async fn claude_adapter_reaps_persistent_process_before_cancelled_stream_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "claude-cancel",
        r#"#!/bin/bash
echo $$ > "$0.pid"
IFS= read -r prompt
: > "$0.prompt"
# Block in the persistent shell itself. Process-tree cleanup has dedicated
# coverage; this test verifies that cancellation awaits this process being
# reaped without depending on a separately scheduled sleep child.
IFS= read -r keepalive
"#,
    );
    let backend = ClaudeBackend::new("claude-code", Some(stub.clone()));
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = start_turn(&backend, || {
        let mut turn = turn(
            tmp.path().to_path_buf(),
            Some("old-sess"),
            BackendPermission::ReadOnly,
        );
        turn.cancel = cancel.clone();
        turn
    })
    .await;
    let drain = tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    });

    let prompt_path = std::path::PathBuf::from(format!("{stub}.prompt"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !prompt_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Claude subprocess should receive the prompt");
    let pid: u32 = std::fs::read_to_string(format!("{stub}.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(6), drain)
        .await
        .expect("cancelled Claude stream should wait for process reaping")
        .unwrap();
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "Claude stream closed before the persistent child was reaped"
    );
}

#[tokio::test]
async fn claude_adapter_disables_tools_for_tool_free_turns() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "claude-tool-free",
        r#"#!/bin/bash
printf '%s\n' "$@" > "$0.args"
cat <<'EOF'
{"type":"result","subtype":"success","session_id":"sess-1","result":"{}","usage":{"input_tokens":1,"output_tokens":1}}
EOF
"#,
    );
    let backend = ClaudeBackend::new("claude-code", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        let mut turn = turn(tmp.path().to_path_buf(), None, BackendPermission::ReadOnly);
        turn.tool_free = true;
        turn
    })
    .await;
    while let Some(event) = stream.next().await {
        event.unwrap();
    }

    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("--tools\n\n"), "{args}");
}

#[tokio::test]
async fn claude_adapter_surfaces_subscription_limit_as_error() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "claude-limit",
        r#"#!/bin/bash
cat <<'EOF'
{"type":"system","subtype":"init","session_id":"sess-limit"}
{"type":"result","subtype":"error_during_execution","is_error":true,"result":"You've hit your usage limit · resets at 3pm"}
EOF
"#,
    );
    let backend = ClaudeBackend::new("claude-code", Some(stub));
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::ReadOnly)
    })
    .await;

    assert!(matches!(
        stream.next().await,
        Some(Ok(BackendEvent::SessionStarted { .. }))
    ));
    let error = stream.next().await.unwrap().unwrap_err().to_string();
    assert!(error.contains("You've hit your usage limit"), "{error}");
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn claude_adapter_reads_subscription_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let soon = chrono::Utc::now().timestamp() + 2 * 3600 + 600;
    let script = format!(
        r#"#!/bin/bash
printf '%s\n' "$@" > "$0.args"
IFS= read -r line
printf '%s\n' "$line" > "$0.request"
cat <<EOF
{{"type":"control_response","response":{{"subtype":"success","request_id":"trouve-usage","response":{{"session":{{"total_cost_usd":0}},"subscription_type":"max","rate_limits_available":true,"rate_limits":{{"five_hour":{{"utilization":62.0,"resets_at":{soon}}},"seven_day":{{"utilization":31.0,"resets_at":{later}}},"extra_usage":{{"is_enabled":false}}}},"behaviors":null}}}}}}
EOF
cat > /dev/null
"#,
        soon = soon,
        later = soon + 86_400,
    );
    let stub = write_stub(tmp.path(), "claude", &script);
    let backend = ClaudeBackend::new("claude-code", Some(stub.clone()));

    let health = backend.subscription_health().await.unwrap();
    assert_eq!(health.status, "ok", "{}", health.note);
    assert_eq!(health.plan, "max");
    assert_eq!(health.windows.len(), 2);
    assert_eq!(health.windows[0].label, "5h window");
    assert_eq!(health.windows[0].used_percent, 62);
    assert!(health.windows[0].resets.starts_with("resets in 2h"));
    assert_eq!(health.windows[1].label, "Weekly (all models)");

    // The query is the sanctioned stream-json `get_usage` control request,
    // sent to a print-mode process that must not touch the user's MCP
    // servers or leave a session transcript behind.
    let request = std::fs::read_to_string(format!("{stub}.request")).unwrap();
    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request["type"], "control_request");
    assert_eq!(request["request"]["subtype"], "get_usage");
    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("-p"), "{args}");
    assert!(args.contains("stream-json"), "{args}");
    assert!(args.contains("--strict-mcp-config"), "{args}");
    assert!(args.contains("--no-session-persistence"), "{args}");
}

/// Minimal HTTP stub for Cursor's dashboard Connect-RPC endpoints: answers
/// GetCurrentPeriodUsage / GetPlanInfo with canned JSON and records each
/// request's path and Authorization header.
fn spawn_dashboard_stub(
    listener: tokio::net::TcpListener,
    seen: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let Ok(n) = sock.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let auth = head
                    .lines()
                    .find_map(|l| {
                        let (name, value) = l.split_once(": ")?;
                        name.eq_ignore_ascii_case("authorization")
                            .then(|| value.trim().to_string())
                    })
                    .unwrap_or_default();
                let body = if path.contains("GetCurrentPeriodUsage") {
                    r#"{"billingCycleEnd":"1782696817000","planUsage":{"totalPercentUsed":35.7,"apiPercentUsed":100,"autoPercentUsed":1.5},"spendLimitUsage":{"individualUsed":241122,"individualLimit":250000}}"#
                } else {
                    r#"{"planInfo":{"planName":"Ultra","price":"$200/mo"}}"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                // Record before responding: once the response is written
                // the client can finish and assert on `seen` before this
                // task gets scheduled again.
                seen.lock().unwrap().push((path, auth));
                let _ = sock.write_all(response.as_bytes()).await;
            });
        }
    });
}

#[tokio::test]
async fn cursor_adapter_reads_dashboard_usage() {
    let dir = tempfile::tempdir().unwrap();
    let auth_file = dir.path().join("auth.json");
    std::fs::write(
        &auth_file,
        r#"{"accessToken":"cli-token","refreshToken":"r"}"#,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    spawn_dashboard_stub(listener, seen.clone());

    let backend = CursorBackend::new("cursor", None, None).with_dashboard(auth_file, base);
    let health = backend.subscription_health().await.unwrap();
    assert_eq!(health.status, "ok", "{}", health.note);
    assert_eq!(health.plan, "Ultra");
    assert_eq!(health.credits, "on-demand: $2411.22 of $2500.00");
    let labels: Vec<&str> = health.windows.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Included usage",
            "Included (API models)",
            "Included (Auto)",
            "On-demand spend"
        ]
    );
    assert_eq!(health.windows[3].used_percent, 96);
    assert_eq!(
        health.windows[0].resets, "resets shortly",
        "cycle end in the past"
    );

    // Both RPCs authenticated with the CLI's stored token, never a refresh.
    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
    assert!(paths.contains(&"/aiserver.v1.DashboardService/GetCurrentPeriodUsage"));
    assert!(paths.contains(&"/aiserver.v1.DashboardService/GetPlanInfo"));
    assert!(
        seen.iter().all(|(_, a)| a == "Bearer cli-token"),
        "{seen:?}"
    );
}

#[tokio::test]
async fn cursor_api_key_backend_reports_no_subscription() {
    // Usage-billed API-key providers have no subscription allowance; the
    // entry explains itself instead of querying the dashboard.
    let backend = CursorBackend::new("cursor-api", None, Some("key-1".into()));
    let health = backend.subscription_health().await.unwrap();
    assert_eq!(health.status, "unsupported");
    assert!(health.note.contains("API key"), "{}", health.note);
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
pwd > "$0.cwd"
echo spawned >> "$0.spawns"
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
IFS= read -r line # session/new
echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}'
IFS= read -r line # set_config_option mode
printf '%s\n' "$line" > "$0.mode"
echo '{"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"mode","currentValue":"agent"}]}}'
IFS= read -r line # set_config_option model
printf '%s\n' "$line" > "$0.model"
echo '{"jsonrpc":"2.0","id":4,"result":{"configOptions":[{"id":"model","currentValue":"test-model"}]}}'
IFS= read -r line # session/prompt
printf '%s\n' "$line" > "$0.prompt"
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Hmm."}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hi "}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"there"}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call","toolCallId":"c1","title":"`ls`","kind":"execute","status":"pending","rawInput":{"command":"ls"}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"in_progress"}}}'
echo '{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"c1","title":"`ls`","kind":"execute"},"options":[{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"allow-always","name":"Allow always","kind":"allow_always"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]}}'
IFS= read -r approval
printf '%s\n' "$approval" > "$0.approval"
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed","rawOutput":{"exitCode":0,"stdout":"a.txt\n"}}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call","toolCallId":"c2","title":"Create Plan","kind":"other","status":"pending","rawInput":{"_toolName":"createPlan"}}}}'
echo '{"jsonrpc":"2.0","id":101,"method":"cursor/create_plan","params":{"toolCallId":"c2","name":"Plan","plan":"# The plan"}}'
IFS= read -r planack
printf '%s\n' "$planack" > "$0.planack"
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"c2","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"tool_call","toolCallId":"c3","title":"Ask Question","kind":"think","status":"pending","rawInput":{"_toolName":"askQuestion"}}}}'
echo '{"jsonrpc":"2.0","id":102,"method":"cursor/ask_question","params":{"toolCallId":"c3","title":"Prefs","questions":[{"id":"q1","prompt":"Color?","options":[{"id":"red","label":"Red"},{"id":"blue","label":"Blue"}],"allowMultiple":false}]}}'
IFS= read -r qans
printf '%s\n' "$qans" > "$0.qans"
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
            args,
            responder,
        } = ev
        {
            assert_eq!(call_id, "c1");
            assert_eq!(tool, "execute");
            assert_eq!(args["rawInput"]["command"], "ls");
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
    let cwd = std::fs::read_to_string(format!("{stub}.cwd")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
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
async fn cursor_adapter_overload_fails_only_the_affected_session() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "cursor-agent-burst",
        r#"#!/bin/bash
read_request() {
    local expected="$1"
    while IFS= read -r line; do
        if [[ "$line" == *"\"id\":$expected"* ]]; then
            return
        fi
    done
}
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'

IFS= read -r line # first session/new
echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}'
IFS= read -r line # first set mode
echo '{"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"mode","currentValue":"agent"}]}}'
IFS= read -r line # first set model
echo '{"jsonrpc":"2.0","id":4,"result":{"configOptions":[{"id":"model","currentValue":"test-model"}]}}'
IFS= read -r line # first session/prompt
# Keep this above both ROUTE_EVENT_BUDGET and BACKEND_BUFFER_MAX_ITEMS.
for sequence in $(seq 1 4096); do
    if (( sequence % 2 )); then
        update="agent_message_chunk"
    else
        update="agent_thought_chunk"
    fi
    printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"%s","content":{"type":"text","text":"%s"}}}}\n' "$update" "$sequence"
done
echo '{"jsonrpc":"2.0","id":5,"result":{"stopReason":"end_turn"}}'

read_request 6 # second session/new; ignore first session's cancel notification
echo '{"jsonrpc":"2.0","id":6,"result":{"sessionId":"sess-2"}}'
read_request 7 # second set mode
echo '{"jsonrpc":"2.0","id":7,"result":{"configOptions":[{"id":"mode","currentValue":"agent"}]}}'
read_request 8 # second set model
echo '{"jsonrpc":"2.0","id":8,"result":{"configOptions":[{"id":"model","currentValue":"test-model"}]}}'
read_request 9 # second session/prompt
echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-2","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second"}}}}'
echo '{"jsonrpc":"2.0","id":9,"result":{"stopReason":"end_turn"}}'
"#,
    );
    let backend = CursorBackend::new("cursor", Some(stub), None);
    let deadline = std::time::Duration::from_secs(2);

    // Leave the first stream unread and drive it past both bounded ingestion
    // layers. Alternating delta kinds prevents the provider-neutral coalescer
    // from collapsing this deliberate overload fixture; neither its
    // backpressure nor the route overload may block the shared reader.
    let mut first = tokio::time::timeout(
        deadline,
        start_turn(&backend, || {
            turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
        }),
    )
    .await
    .expect("first turn should start");
    let mut second = tokio::time::timeout(
        deadline,
        start_turn(&backend, || {
            turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
        }),
    )
    .await
    .expect("a burst on one route must not block another session");

    let mut second_text = String::new();
    while let Some(event) = tokio::time::timeout(deadline, second.next())
        .await
        .expect("second session must keep streaming")
    {
        if let BackendEvent::TextDelta(text) = event.unwrap() {
            second_text.push_str(&text);
        }
    }
    assert_eq!(second_text, "second");

    let mut first_error = None;
    while let Some(event) = tokio::time::timeout(deadline, first.next())
        .await
        .expect("overloaded first session must terminate")
    {
        if let Err(error) = event {
            first_error = Some(error.to_string());
        }
    }
    assert!(
        first_error
            .as_deref()
            .is_some_and(|error| error.contains("event backlog exceeded")),
        "{first_error:?}"
    );
}

#[tokio::test]
async fn cursor_adapter_rejects_request_that_overflows_its_route() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "cursor-agent-request-overload",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
IFS= read -r line # session/new
echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}'
IFS= read -r line # set mode
echo '{"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"mode","currentValue":"agent"}]}}'
IFS= read -r line # set model
echo '{"jsonrpc":"2.0","id":4,"result":{"configOptions":[{"id":"model","currentValue":"test-model"}]}}'
IFS= read -r line # session/prompt
echo '{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"c1","title":"first","kind":"execute"},"options":[{"optionId":"allow-once","kind":"allow_once"},{"optionId":"reject-once","kind":"reject_once"}]}}'
while [[ ! -f "$0.continue" ]]; do sleep 0.01; done
for sequence in $(seq 1 1024); do
    printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"%s"}}}}\n' "$sequence"
done
echo '{"jsonrpc":"2.0","id":101,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"c2","title":"overflow","kind":"execute"},"options":[{"optionId":"allow-once","kind":"allow_once"},{"optionId":"reject-once","kind":"reject_once"}]}}'
received_dropped=false
received_cancel=false
while [[ "$received_dropped" != true || "$received_cancel" != true ]] && IFS= read -r message; do
    if [[ "$message" == *'"id":101'* ]]; then
        printf '%s\n' "$message" > "$0.dropped.tmp"
        mv "$0.dropped.tmp" "$0.dropped"
        received_dropped=true
    fi
    if [[ "$message" == *'"method":"session/cancel"'* ]]; then
        echo '{"jsonrpc":"2.0","id":5,"result":{"stopReason":"cancelled"}}'
        received_cancel=true
    fi
done
cat > /dev/null
"#,
    );
    let backend = CursorBackend::new("cursor", Some(stub.clone()), None);
    let deadline = std::time::Duration::from_secs(2);
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let held_responder = loop {
        let event = tokio::time::timeout(deadline, stream.next())
            .await
            .expect("first permission request must arrive")
            .expect("stream must remain open")
            .expect("first permission request must be valid");
        if let BackendEvent::ApprovalNeeded { responder, .. } = event {
            break responder;
        }
    };
    std::fs::write(format!("{stub}.continue"), "").unwrap();

    let dropped_path = std::path::PathBuf::from(format!("{stub}.dropped"));
    tokio::time::timeout(deadline, async {
        while !dropped_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("overflowed request must receive a JSON-RPC error");
    let dropped: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dropped_path).unwrap()).unwrap();
    assert_eq!(dropped["id"], 101);
    assert_eq!(dropped["error"]["code"], -32603);
    assert_eq!(
        dropped["error"]["message"],
        "session event route unavailable"
    );
    let mut overload_error = None;
    // Overload cleanup may spend up to five seconds waiting for Cursor's
    // cancellation acknowledgement before entering its bounded process-reap
    // fallback. Keep the short setup deadline above, but cover that complete
    // production shutdown contract here.
    let shutdown_deadline = std::time::Duration::from_secs(12);
    while let Some(event) = tokio::time::timeout(shutdown_deadline, stream.next())
        .await
        .expect("overloaded Cursor stream must terminate")
    {
        if let Err(error) = event {
            overload_error = Some(error.to_string());
        }
    }
    drop(held_responder);
    assert!(
        overload_error
            .as_deref()
            .is_some_and(|error| error.contains("event backlog exceeded")),
        "{overload_error:?}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cursor_adapter_recycles_process_tree_when_eof_closes_cancel_waiter() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "cursor-agent-eof-overload",
        r#"#!/bin/bash
spawns=$(($(cat "$0.spawns" 2>/dev/null || echo 0) + 1))
printf '%s\n' "$spawns" > "$0.spawns.tmp"
mv "$0.spawns.tmp" "$0.spawns"
if (( spawns == 1 )); then
    sleep 60 </dev/null >/dev/null 2>&1 &
    printf '%s\n' "$!" > "$0.descendant.tmp"
    mv "$0.descendant.tmp" "$0.descendant"
fi
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
IFS= read -r line # session/new
echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}'
IFS= read -r line # set mode
echo '{"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"mode","currentValue":"agent"}]}}'
IFS= read -r line # set model
echo '{"jsonrpc":"2.0","id":4,"result":{"configOptions":[{"id":"model","currentValue":"test-model"}]}}'
IFS= read -r line # session/prompt
if (( spawns > 1 )); then
    echo '{"jsonrpc":"2.0","id":5,"result":{"stopReason":"end_turn"}}'
    cat > /dev/null
    exit 0
fi
echo '{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"sessionId":"sess-1","toolCall":{"toolCallId":"c1","title":"hold route","kind":"execute"},"options":[{"optionId":"allow-once","kind":"allow_once"},{"optionId":"reject-once","kind":"reject_once"}]}}'
while [[ ! -f "$0.continue" ]]; do sleep 0.01; done
for sequence in $(seq 1 1025); do
    printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"%s"}}}}\n' "$sequence"
done
while IFS= read -r cancel; do
    if [[ "$cancel" == *'"method":"session/cancel"'* ]]; then
        printf '%s\n' "$cancel" > "$0.cancel.tmp"
        mv "$0.cancel.tmp" "$0.cancel"
        # Exit without a session/prompt response. Reader EOF clears the
        # pending sender; that closed oneshot is not an acknowledgement.
        exit 0
    fi
done
"#,
    );
    let backend = CursorBackend::new("cursor", Some(stub.clone()), None);
    let deadline = std::time::Duration::from_secs(3);
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let held_responder = loop {
        let event = tokio::time::timeout(deadline, stream.next())
            .await
            .expect("permission request must arrive")
            .expect("stream must remain open")
            .expect("permission request must be valid");
        if let BackendEvent::ApprovalNeeded { responder, .. } = event {
            break responder;
        }
    };
    let descendant_path = PathBuf::from(format!("{stub}.descendant"));
    let descendant = tokio::time::timeout(deadline, async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&descendant_path)
                && let Ok(pid) = pid.trim().parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Cursor stub did not publish its descendant pid");
    std::fs::write(format!("{stub}.continue"), "").unwrap();

    let mut overload_error = None;
    while let Some(event) = tokio::time::timeout(deadline, stream.next())
        .await
        .expect("overloaded stream must terminate after EOF cleanup")
    {
        if let Err(error) = event {
            overload_error = Some(error.to_string());
        }
    }
    drop(held_responder);
    assert!(
        overload_error
            .as_deref()
            .is_some_and(|error| error.contains("event backlog exceeded")),
        "{overload_error:?}"
    );
    assert!(PathBuf::from(format!("{stub}.cancel")).exists());
    wait_for_process_to_stop(descendant).await;

    // The closed transport must be removed from the pool, not reused.
    let mut replacement = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;
    while let Some(event) = tokio::time::timeout(deadline, replacement.next())
        .await
        .expect("replacement Cursor process must finish")
    {
        event.unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(format!("{stub}.spawns"))
            .unwrap()
            .trim(),
        "2"
    );
}

#[tokio::test]
async fn cursor_adapter_releases_requests_when_the_transport_exits() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "cursor-agent-exits",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
IFS= read -r line # session/new, then exit without responding
"#,
    );
    let backend = CursorBackend::new("cursor", Some(stub), None);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        backend.run_turn(turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)),
    )
    .await
    .expect("transport EOF must release the pending session/new request");

    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("the interrupted request should fail"),
    };
    assert!(
        error
            .to_string()
            .contains("cursor-agent closed before responding"),
        "{error}"
    );
}

#[tokio::test]
async fn cursor_adapter_waits_for_prompt_cancellation_acknowledgement() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "cursor-agent-cancel-ack",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
IFS= read -r line # session/new
echo '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}'
IFS= read -r line # set mode
echo '{"jsonrpc":"2.0","id":3,"result":{"configOptions":[{"id":"mode","currentValue":"agent"}]}}'
IFS= read -r line # set model
echo '{"jsonrpc":"2.0","id":4,"result":{"configOptions":[{"id":"model","currentValue":"test-model"}]}}'
IFS= read -r line # session/prompt (id 5)
while IFS= read -r cancel; do
    if [[ "$cancel" == *'"method":"session/cancel"'* ]]; then
        printf '%s\n' "$cancel" > "$0.cancel.tmp"
        mv "$0.cancel.tmp" "$0.cancel"
        while [[ ! -f "$0.release" ]]; do sleep 0.01; done
        echo '{"jsonrpc":"2.0","id":5,"result":{"stopReason":"cancelled"}}'
        break
    fi
done
cat > /dev/null
"#,
    );
    let backend = CursorBackend::new("cursor", Some(stub.clone()), None);
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = start_turn(&backend, || {
        let mut turn = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
        turn.cancel = cancel.clone();
        turn
    })
    .await;

    cancel.cancel();
    let drain = tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    });
    let cancel_path = std::path::PathBuf::from(format!("{stub}.cancel"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !cancel_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Cursor should receive session/cancel");
    assert!(
        !drain.is_finished(),
        "backend stream closed before session/prompt acknowledged cancellation"
    );

    std::fs::write(format!("{stub}.release"), "").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), drain)
        .await
        .expect("stream should close after Cursor acknowledges cancellation")
        .unwrap();
}

#[tokio::test]
async fn cursor_adapter_maps_permissions_to_safe_modes() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = cursor_acp_stub(tmp.path());
    let backend = CursorBackend::new("cursor", Some(stub.clone()), None);
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Yolo)
    })
    .await;

    // Yolo still surfaces the internal approval event so the engine can
    // reject an out-of-worktree target. The engine auto-approves safe calls,
    // represented here by replying true; no user prompt is created.
    let mut saw_approval = false;
    let mut completed = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            BackendEvent::ApprovalNeeded { responder, .. } => {
                saw_approval = true;
                responder.send(true).unwrap();
            }
            BackendEvent::Completed { .. } => completed = true,
            _ => {}
        }
    }
    assert!(saw_approval);
    assert!(completed);
    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("allow-once"), "{reply}");

    // A different worktree gets a different cwd-pinned child from the same
    // backend pool; it cannot inherit or reuse the first child's cwd.
    let other_worktree = tempfile::tempdir().unwrap();
    let mut other = start_turn(&backend, || {
        turn(
            other_worktree.path().to_path_buf(),
            None,
            BackendPermission::Yolo,
        )
    })
    .await;
    while let Some(ev) = other.next().await {
        if let BackendEvent::ApprovalNeeded { responder, .. } = ev.unwrap() {
            responder.send(true).unwrap();
        }
    }
    let cwd = std::fs::read_to_string(format!("{stub}.cwd")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        other_worktree.path().canonicalize().unwrap()
    );
    let spawns = std::fs::read_to_string(format!("{stub}.spawns")).unwrap();
    assert_eq!(spawns.lines().count(), 2, "{spawns}");
    let mode = std::fs::read_to_string(format!("{stub}.mode")).unwrap();
    assert!(mode.contains("\"value\":\"agent\""), "{mode}");

    // Read-only turns use Cursor Ask mode (search-only); approval-gated turns
    // retain agent mode. Use fresh backends because the ACP child is pooled
    // per worktree.
    for (permission, expected_mode) in [
        (BackendPermission::ReadOnly, "ask"),
        (BackendPermission::Ask, "agent"),
    ] {
        let permission_worktree = tempfile::tempdir().unwrap();
        let permission_stub = cursor_acp_stub(permission_worktree.path());
        let permission_backend = CursorBackend::new("cursor", Some(permission_stub.clone()), None);
        let mut permission_stream = start_turn(&permission_backend, || {
            turn(permission_worktree.path().to_path_buf(), None, permission)
        })
        .await;
        while let Some(event) = permission_stream.next().await {
            if let BackendEvent::ApprovalNeeded { responder, .. } = event.unwrap() {
                let _ = responder.send(false);
            }
        }
        let mode = std::fs::read_to_string(format!("{permission_stub}.mode")).unwrap();
        assert!(
            mode.contains(&format!("\"value\":\"{expected_mode}\"")),
            "{permission:?}: {mode}"
        );
    }
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
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
printf '%s\n' "$line" > "$0.thread-start"
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # turn/start
printf '%s\n' "$line" > "$0.turn-start"
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"thr-1","item":{"id":"thinking-1","type":"agentMessage","text":"","phase":"commentary"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","itemId":"thinking-1","delta":"Checking the workspace."}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"threadId":"thr-1","item":{"id":"thinking-1","type":"agentMessage","text":"Checking the workspace.","phase":"commentary"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"thr-1","item":{"id":"i1","type":"agentMessage","text":"","phase":"final_answer"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","itemId":"i1","delta":"Hello"}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"thr-1","item":{"id":"reasoning-1","type":"reasoning","summary":[],"content":[]}}}'
echo '{"jsonrpc":"2.0","method":"item/reasoning/textDelta","params":{"threadId":"thr-1","itemId":"reasoning-1","delta":"Raw reasoning."}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"threadId":"thr-1","item":{"id":"reasoning-1","type":"reasoning","summary":[],"content":["Raw reasoning."]}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"thr-1","item":{"id":"compact-1","type":"contextCompaction"}}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"threadId":"thr-1","item":{"id":"compact-1","type":"contextCompaction","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"thr-1","item":{"id":"c1","type":"commandExecution","command":"ls"}}}'
echo '{"jsonrpc":"2.0","id":100,"method":"item/commandExecution/requestApproval","params":{"threadId":"thr-1","itemId":"c1","command":"ls"}}'
IFS= read -r approval
printf '%s\n' "$approval" > "$0.approval"
echo '{"jsonrpc":"2.0","method":"item/commandExecution/outputDelta","params":{"threadId":"thr-1","itemId":"c1","delta":"a.txt\n"}}'
echo '{"jsonrpc":"2.0","method":"item/completed","params":{"threadId":"thr-1","item":{"id":"c1","type":"commandExecution","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"turn/plan/updated","params":{"threadId":"thr-1","turnId":"turn-1","explanation":"Implementation plan","plan":[{"step":"Inspect the adapter","status":"completed"},{"step":"Publish the todo snapshot","status":"inProgress"},{"step":"Verify the pane","status":"pending"}]}}'
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

    let mut thinking = Vec::new();
    let mut thinking_completed = 0;
    let mut progress = Vec::new();
    let mut progress_completed = 0;
    let mut saw_text = false;
    let mut saw_tool_started = false;
    let mut saw_tool_output = false;
    let mut saw_tool_completed = false;
    let mut saw_compaction_started = false;
    let mut saw_compaction_completed = false;
    let mut todos = None;
    let mut sessions = Vec::new();
    let mut live_usage = None;
    let mut usage = None;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            BackendEvent::SessionStarted { session_id } => sessions.push(session_id),
            BackendEvent::ThinkingDelta(t) => thinking.push(t),
            BackendEvent::ThinkingCompleted => thinking_completed += 1,
            BackendEvent::ProgressDelta(t) => progress.push(t),
            BackendEvent::ProgressCompleted => progress_completed += 1,
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
            BackendEvent::UsageUpdated { usage } => live_usage = Some(usage),
            BackendEvent::Completed { usage: u } => usage = Some(u),
            BackendEvent::CompactionStarted => saw_compaction_started = true,
            BackendEvent::CompactionCompleted => saw_compaction_completed = true,
            BackendEvent::TodosUpdated { todos: updated } => todos = Some(updated),
            BackendEvent::CollaboratorStarted { .. } | BackendEvent::CollaboratorEvent { .. } => {
                panic!("single-threaded adapter fixture emitted a collaborator event")
            }
            BackendEvent::QuestionsNeeded { .. }
            | BackendEvent::CommandsUpdated { .. }
            | BackendEvent::CompactionFailed => {}
        }
    }

    assert_eq!(sessions, vec!["thr-1"]);
    assert_eq!(progress, ["Checking the workspace."]);
    assert_eq!(progress_completed, 1);
    assert_eq!(thinking_completed, 1);
    assert_eq!(
        thinking
            .iter()
            .filter(|text| text.as_str() == "Raw reasoning.")
            .count(),
        1,
        "completed reasoning must not repeat a streamed raw delta: {thinking:?}"
    );
    assert!(saw_text && saw_tool_started && saw_tool_output && saw_tool_completed);
    assert!(saw_compaction_started && saw_compaction_completed);
    let todos = todos.expect("Codex plan update");
    assert_eq!(todos.len(), 3);
    assert_eq!(todos[0].id, "codex-plan:19:Inspect the adapter:1");
    assert_eq!(todos[0].status, trouve_protocol::TodoStatus::Completed);
    assert_eq!(todos[1].content, "Publish the todo snapshot");
    assert_eq!(todos[1].status, trouve_protocol::TodoStatus::InProgress);
    assert_eq!(todos[2].status, trouve_protocol::TodoStatus::Pending);
    assert_eq!(
        live_usage.expect("live usage").context_input_tokens,
        Some(11)
    );
    let usage = usage.expect("turn completed");
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 4);
    assert_eq!(usage.context_input_tokens, Some(11));

    // Our approval reply reached the vendor with an accept decision.
    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("\"id\":100"), "{reply}");
    assert!(reply.contains("\"decision\":\"accept\""), "{reply}");

    // Ask mode keeps Codex's command approval gate, but runs approved work
    // without its workspace sandbox so linked-worktree Git metadata remains
    // writable (ADR 0004 leaves local isolation to trouve's permissions).
    let thread_start = std::fs::read_to_string(format!("{stub}.thread-start")).unwrap();
    let thread_start: serde_json::Value = serde_json::from_str(&thread_start).unwrap();
    assert_eq!(thread_start["params"]["approvalPolicy"], "untrusted");
    assert_eq!(thread_start["params"]["sandbox"], "danger-full-access");
    assert_eq!(
        thread_start["params"]["config"]["show_raw_agent_reasoning"],
        true
    );
    assert_eq!(
        thread_start["params"]["developerInstructions"],
        "mode prompt"
    );
    let turn_start = std::fs::read_to_string(format!("{stub}.turn-start")).unwrap();
    let turn_start: serde_json::Value = serde_json::from_str(&turn_start).unwrap();
    assert_eq!(turn_start["params"]["approvalPolicy"], "untrusted");
    assert_eq!(turn_start["params"]["summary"], "none");
    assert_eq!(
        turn_start["params"]["input"][0]["text"], "do the thing",
        "mode instructions belong in developerInstructions, not user input"
    );
    assert_eq!(
        turn_start["params"]["sandboxPolicy"],
        serde_json::json!({ "type": "dangerFullAccess" })
    );
}

#[tokio::test]
async fn codex_adapter_refreshes_changed_instructions_before_reusing_a_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-cold-resume-instructions",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # first thread/resume
printf '%s\n' "$line" > "$0.thread-resume-1"
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # first turn/start
printf '%s\n' "$line" > "$0.turn-start-1"
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
IFS= read -r line # first thread/unsubscribe
echo '{"jsonrpc":"2.0","id":4,"result":{}}'
IFS= read -r line # second thread/resume after developer instructions change
printf '%s\n' "$line" > "$0.thread-resume-2"
echo '{"jsonrpc":"2.0","id":5,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # second turn/start
printf '%s\n' "$line" > "$0.turn-start-2"
echo '{"jsonrpc":"2.0","id":6,"result":{"turn":{"id":"turn-2"}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-2","status":"completed"}}}'
IFS= read -r line # second thread/unsubscribe
echo '{"jsonrpc":"2.0","id":7,"result":{}}'
IFS= read -r line # third thread/resume after terminal release
printf '%s\n' "$line" > "$0.thread-resume-3"
echo '{"jsonrpc":"2.0","id":8,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # third turn/start
printf '%s\n' "$line" > "$0.turn-start-3"
echo '{"jsonrpc":"2.0","id":9,"result":{"turn":{"id":"turn-3"}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-3","status":"completed"}}}'
IFS= read -r line # third thread/unsubscribe
echo '{"jsonrpc":"2.0","id":10,"result":{}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    for instructions in ["mode prompt", "updated mode prompt", "updated mode prompt"] {
        let mut stream = start_turn(&backend, || {
            let mut request = turn(
                tmp.path().to_path_buf(),
                Some("thr-1"),
                BackendPermission::Ask,
            );
            request.instructions = Some(instructions.into());
            request
        })
        .await;
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    }

    let first_resume: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.thread-resume-1")).unwrap())
            .unwrap();
    assert_eq!(
        first_resume["params"]["developerInstructions"],
        "mode prompt"
    );
    let first_turn: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.turn-start-1")).unwrap())
            .unwrap();
    assert_eq!(
        first_turn["params"]["input"][0]["text"],
        "<mode-instructions>\nmode prompt\n</mode-instructions>\n\ndo the thing",
        "the first cold-resumed request needs a prompt fallback for Codex versions that delay developer-instruction overrides"
    );
    let second_resume: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.thread-resume-2")).unwrap())
            .unwrap();
    assert_eq!(
        second_resume["params"]["developerInstructions"], "updated mode prompt",
        "changed thread-level instructions must force a resume before the next turn"
    );
    let second_turn: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.turn-start-2")).unwrap())
            .unwrap();
    assert_eq!(
        second_turn["params"]["input"][0]["text"],
        "<mode-instructions>\nupdated mode prompt\n</mode-instructions>\n\ndo the thing",
        "the first turn after an instruction change keeps the compatibility fallback"
    );
    let third_turn: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.turn-start-3")).unwrap())
            .unwrap();
    assert_eq!(
        third_turn["params"]["input"][0]["text"], "do the thing",
        "once the refreshed instructions have reached a turn, later user prompts stay clean"
    );
}

#[tokio::test]
async fn codex_adapter_routes_spawned_agent_requests_through_the_parent_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-child-request",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"root"}}}'
IFS= read -r line # turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"root-turn"}}}'
# Codex can multiplex a child request before its collaboration item reaches
# the parent stream. The adapter must replay it once ownership is announced.
echo '{"jsonrpc":"2.0","id":100,"method":"item/commandExecution/requestApproval","params":{"threadId":"child","turnId":"child-turn","itemId":"child-command","command":"pwd"}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"spawn-1","type":"collabToolCall","tool":"spawn_agent","senderThreadId":"root","newThreadId":"child"}}}'
IFS= read -r prompt_lookup
printf '%s\n' "$prompt_lookup" > "$0.prompt-lookup"
# The collaboration announcement can race the child turn becoming visible.
# Return an empty first page and expose the real Responses-style prompt on the
# bounded retry.
echo '{"jsonrpc":"2.0","id":4,"result":{"data":[]}}'
# Prompt recovery no longer blocks child approvals, so these two client writes
# may arrive in either order. Dispatch them by identity instead of assuming
# the retry wins the race.
lookup_done=0
approval_done=0
while (( !lookup_done || !approval_done )); do
    IFS= read -r client_message
    if [[ "$client_message" == *'"method":"thread/turns/list"'* ]]; then
        printf '%s\n' "$client_message" >> "$0.prompt-lookup"
        echo '{"jsonrpc":"2.0","id":5,"result":{"data":[{"id":"child-turn","items":[{"type":"userMessage","content":[{"type":"input_text","text":"Inspect the child task."}]}]}]}}'
        lookup_done=1
    elif [[ "$client_message" == *'"id":100'* ]]; then
        printf '%s\n' "$client_message" > "$0.approval"
        approval_done=1
    fi
done
# Inter-agent communication from a child back to the root is activity, not a
# nested spawn. Older routing treated the ancestor id as a fresh collaborator
# and then waited forever for that phantom child to complete.
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"child","turnId":"child-turn","item":{"id":"child-to-root","type":"subAgentActivity","agentThreadId":"root","agentPath":"/root","kind":"interacted"}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"child","turn":{"id":"child-turn","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"followup-1","type":"collabToolCall","tool":"resume_agent","senderThreadId":"root","receiverThreadId":"child","prompt":"Double-check the result."}}}'
echo '{"jsonrpc":"2.0","method":"turn/started","params":{"threadId":"child","turn":{"id":"child-turn-2","status":"inProgress"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"child","turnId":"child-turn-2","item":{"id":"child-prompt-2","type":"userMessage","content":[{"type":"input_text","text":"Double-check the result."}]}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"child","turnId":"child-turn-2","itemId":"child-answer-2","delta":"Child checked again."}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"child","turn":{"id":"child-turn-2","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"root","turnId":"root-turn","itemId":"answer","delta":"Parent finished."}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}'
IFS= read -r unsubscribe
echo '{"jsonrpc":"2.0","id":6,"result":{}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let mut saw_parent_text = false;
    let mut child_announcements = 0;
    let mut saw_initial_child_prompt = false;
    let mut child_completions = 0;
    let mut saw_child_followup = false;
    let mut saw_child_text = false;
    let mut completed = false;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                BackendEvent::CollaboratorStarted {
                    session_id, prompt, ..
                } => {
                    assert_eq!(session_id, "child", "an ancestor was projected as a child");
                    child_announcements += 1;
                    saw_initial_child_prompt |=
                        prompt.as_deref() == Some("Inspect the child task.");
                }
                BackendEvent::CollaboratorEvent {
                    session_id,
                    turn_id: None,
                    event: BackendCollaboratorEvent::UserMessage(content),
                } => {
                    assert_eq!(session_id, "child");
                    saw_initial_child_prompt |= content == "Inspect the child task.";
                }
                BackendEvent::CollaboratorEvent {
                    session_id,
                    turn_id,
                    event:
                        BackendCollaboratorEvent::ApprovalNeeded {
                            call_id,
                            tool,
                            responder,
                            ..
                        },
                    ..
                } => {
                    assert_eq!(session_id, "child");
                    assert_eq!(turn_id.as_deref(), Some("child-turn"));
                    assert_eq!(call_id, "child-command");
                    assert_eq!(tool, "commandExecution");
                    responder.send(true).unwrap();
                }
                BackendEvent::CollaboratorEvent {
                    session_id,
                    turn_id,
                    event: BackendCollaboratorEvent::Completed { .. },
                    ..
                } => {
                    if session_id == "child" {
                        assert!(matches!(
                            turn_id.as_deref(),
                            Some("child-turn" | "child-turn-2")
                        ));
                        child_completions += 1;
                    }
                }
                BackendEvent::CollaboratorEvent {
                    turn_id: Some(turn_id),
                    event: BackendCollaboratorEvent::UserMessage(content),
                    ..
                } => {
                    saw_child_followup |=
                        turn_id == "child-turn-2" && content == "Double-check the result.";
                }
                BackendEvent::CollaboratorEvent {
                    turn_id: Some(turn_id),
                    event: BackendCollaboratorEvent::TextDelta(text),
                    ..
                } => {
                    saw_child_text |= turn_id == "child-turn-2" && text == "Child checked again.";
                }
                BackendEvent::TextDelta(text) => saw_parent_text |= text == "Parent finished.",
                BackendEvent::Completed { .. } => {
                    assert!(
                        saw_initial_child_prompt,
                        "root completion overtook collaborator prompt recovery"
                    );
                    completed = true;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the root turn should finish after its real child completes");

    assert_eq!(child_announcements, 2);
    assert!(saw_initial_child_prompt);
    assert_eq!(child_completions, 2);
    assert!(saw_child_followup && saw_child_text && saw_parent_text && completed);
    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("\"id\":100"), "{reply}");
    assert!(reply.contains("\"decision\":\"accept\""), "{reply}");
    let lookup = std::fs::read_to_string(format!("{stub}.prompt-lookup")).unwrap();
    assert!(
        lookup.contains("\"method\":\"thread/turns/list\""),
        "{lookup}"
    );
    assert!(lookup.contains("\"threadId\":\"child\""), "{lookup}");
    assert_eq!(
        lookup.lines().count(),
        2,
        "empty collaborator pages must be retried"
    );
}

#[tokio::test]
async fn codex_adapter_holds_root_completion_for_late_prompt_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-late-child-prompt",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"root"}}}'
IFS= read -r line # turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"root-turn"}}}'
echo '{"jsonrpc":"2.0","method":"item/started","params":{"threadId":"root","turnId":"root-turn","item":{"id":"spawn-1","type":"collabToolCall","tool":"spawn_agent","senderThreadId":"root","newThreadId":"child"}}}'
IFS= read -r prompt_lookup
printf '%s\n' "$prompt_lookup" > "$0.prompt-lookup"
echo '{"jsonrpc":"2.0","id":4,"result":{"data":[]}}'
# Complete both turns while prompt recovery is sleeping before its bounded
# retry. The root terminal event is only a candidate until that lookup lands.
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"child","turn":{"id":"child-turn","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"root","turn":{"id":"root-turn","status":"completed"}}}'
IFS= read -r prompt_lookup_retry
printf '%s\n' "$prompt_lookup_retry" >> "$0.prompt-lookup"
echo '{"jsonrpc":"2.0","id":5,"result":{"data":[{"id":"child-turn","items":[{"type":"userMessage","content":[{"type":"input_text","text":"Recovered after completion."}]}]}]}}'
IFS= read -r unsubscribe
echo '{"jsonrpc":"2.0","id":6,"result":{}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let mut recovered_prompt = false;
    let mut completed = false;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                BackendEvent::CollaboratorEvent {
                    session_id,
                    turn_id: None,
                    event: BackendCollaboratorEvent::UserMessage(prompt),
                } => {
                    assert_eq!(session_id, "child");
                    recovered_prompt = prompt == "Recovered after completion.";
                }
                BackendEvent::Completed { .. } => {
                    assert!(
                        recovered_prompt,
                        "root completion overtook its pending collaborator prompt lookup"
                    );
                    completed = true;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("root turn did not finish after collaborator prompt recovery");

    assert!(recovered_prompt && completed);
    let lookup = std::fs::read_to_string(format!("{stub}.prompt-lookup")).unwrap();
    assert_eq!(lookup.lines().count(), 2, "{lookup}");
}

#[tokio::test]
async fn codex_adapter_sends_decline_when_user_denies_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
echo '{"jsonrpc":"2.0","id":100,"method":"item/commandExecution/requestApproval","params":{"threadId":"thr-1","itemId":"c1","command":"ls"}}'
IFS= read -r approval
printf '%s\n' "$approval" > "$0.approval"
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
IFS= read -r unsubscribe
echo '{"jsonrpc":"2.0","id":4,"result":{}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    while let Some(ev) = stream.next().await {
        if let BackendEvent::ApprovalNeeded { responder, .. } = ev.unwrap() {
            responder.send(false).unwrap();
        }
    }

    let reply = std::fs::read_to_string(format!("{stub}.approval")).unwrap();
    assert!(reply.contains("\"id\":100"), "{reply}");
    assert!(reply.contains("\"decision\":\"decline\""), "{reply}");
}

#[tokio::test]
async fn codex_adapter_interrupts_overloaded_turn_with_pending_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-overload",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
echo '{"jsonrpc":"2.0","id":100,"method":"item/commandExecution/requestApproval","params":{"threadId":"thr-1","turnId":"turn-1","itemId":"c1","command":"ls"}}'
for sequence in $(seq 1 2048); do
    printf '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-1","delta":"%s"}}\n' "$sequence"
done
while IFS= read -r interrupt; do
    if [[ "$interrupt" == *'"method":"turn/interrupt"'* ]]; then
        printf '%s\n' "$interrupt" > "$0.interrupt.tmp"
        mv "$0.interrupt.tmp" "$0.interrupt"
        break
    fi
done
echo '{"jsonrpc":"2.0","id":4,"result":{}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let deadline = std::time::Duration::from_secs(2);
    let mut stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let interrupt_path = std::path::PathBuf::from(format!("{stub}.interrupt"));
    tokio::time::timeout(deadline, async {
        while !interrupt_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("overloaded Codex turn must be interrupted");

    let interrupt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(interrupt_path).unwrap()).unwrap();
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"]["threadId"], "thr-1");
    assert_eq!(interrupt["params"]["turnId"], "turn-1");

    let mut overload_error = None;
    while let Some(event) = tokio::time::timeout(deadline, stream.next())
        .await
        .expect("overloaded Codex stream must terminate")
    {
        if let Err(error) = event {
            overload_error = Some(error.to_string());
        }
    }
    assert!(
        overload_error
            .as_deref()
            .is_some_and(|error| error.contains("event backlog exceeded")),
        "{overload_error:?}"
    );
}

#[tokio::test]
async fn codex_adapter_interrupts_turn_when_stream_is_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-cancel",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
while IFS= read -r interrupt; do
    if [[ "$interrupt" == *'"method":"turn/interrupt"'* ]]; then
        printf '%s\n' "$interrupt" > "$0.interrupt.tmp"
        mv "$0.interrupt.tmp" "$0.interrupt"
        echo '{"jsonrpc":"2.0","id":4,"result":{}}'
        break
    fi
done
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let stream = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    drop(stream);

    let deadline = std::time::Duration::from_secs(2);
    let interrupt_path = std::path::PathBuf::from(format!("{stub}.interrupt"));
    tokio::time::timeout(deadline, async {
        while !interrupt_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping a Codex stream must interrupt its vendor turn");

    let interrupt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(interrupt_path).unwrap()).unwrap();
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"]["threadId"], "thr-1");
    assert_eq!(interrupt["params"]["turnId"], "turn-1");
}

#[tokio::test]
async fn codex_adapter_keeps_cancelled_stream_open_until_interrupt_is_acknowledged() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-cancel-ack",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
while IFS= read -r interrupt; do
    if [[ "$interrupt" == *'"method":"turn/interrupt"'* ]]; then
        printf '%s\n' "$interrupt" > "$0.interrupt.tmp"
        mv "$0.interrupt.tmp" "$0.interrupt"
        while [[ ! -f "$0.release" ]]; do sleep 0.01; done
        echo '{"jsonrpc":"2.0","id":4,"result":{}}'
        break
    fi
done
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = start_turn(&backend, || {
        let mut turn = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
        turn.cancel = cancel.clone();
        turn
    })
    .await;

    cancel.cancel();
    let drain = tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
    });
    let interrupt_path = std::path::PathBuf::from(format!("{stub}.interrupt"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !interrupt_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancellation should interrupt the exact vendor turn");
    assert!(
        !drain.is_finished(),
        "backend stream closed before turn/interrupt was acknowledged"
    );

    std::fs::write(format!("{stub}.release"), "").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), drain)
        .await
        .expect("stream should close after interrupt acknowledgement")
        .unwrap();
}

#[tokio::test]
async fn codex_adapter_ignores_late_events_from_cancelled_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-late-events",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # first turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
IFS= read -r line # first turn/interrupt
printf '%s\n' "$line" > "$0.interrupt.tmp"
mv "$0.interrupt.tmp" "$0.interrupt"
echo '{"jsonrpc":"2.0","id":4,"result":{}}'
IFS= read -r line # replacement turn/start
echo '{"jsonrpc":"2.0","id":5,"result":{"turn":{"id":"turn-2"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-1","delta":"stale"}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-2","delta":"replacement"}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-2","status":"completed"}}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let first = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;
    drop(first);

    let interrupt_path = std::path::PathBuf::from(format!("{stub}.interrupt"));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !interrupt_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled turn must be interrupted before its replacement starts");

    let mut replacement = start_turn(&backend, || {
        turn(
            tmp.path().to_path_buf(),
            Some("thr-1"),
            BackendPermission::Ask,
        )
    })
    .await;
    let mut text = String::new();
    let mut completed = 0;
    while let Some(event) = replacement.next().await {
        match event.unwrap() {
            BackendEvent::TextDelta(delta) => text.push_str(&delta),
            BackendEvent::Completed { .. } => completed += 1,
            _ => {}
        }
    }

    assert_eq!(text, "replacement");
    assert_eq!(completed, 1);
    assert!(
        !std::path::Path::new(&format!("{stub}.thread-resume")).exists(),
        "a thread already loaded with matching MCP configuration must be reused"
    );
}

#[tokio::test]
async fn codex_adapter_aborts_replacement_when_predecessor_interrupt_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-interrupt-failure",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # first turn/start
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
IFS= read -r line # predecessor turn/interrupt
printf '%s\n' "$line" > "$0.interrupt"
echo '{"jsonrpc":"2.0","id":4,"error":{"message":"cannot interrupt predecessor"}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
IFS= read -r unsubscribe
echo '{"jsonrpc":"2.0","id":5,"result":{}}'
cat > /dev/null
"#,
    );
    let backend = CodexBackend::new("codex", Some(stub.clone()));
    let mut first = start_turn(&backend, || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    let replacement = backend
        .run_turn(turn(
            tmp.path().to_path_buf(),
            Some("thr-1"),
            BackendPermission::Ask,
        ))
        .await;
    let error = match replacement {
        Ok(_) => panic!("replacement must not start after an interrupt failure"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("cannot interrupt predecessor"), "{error}");

    let interrupt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.interrupt")).unwrap())
            .unwrap();
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"]["turnId"], "turn-1");

    let first_completed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match first.next().await {
                Some(Ok(BackendEvent::Completed { .. })) => break true,
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("interrupt rejection killed the transport: {error}"),
                None => break false,
            }
        }
    })
    .await
    .expect("the predecessor completion must remain routable");
    assert!(first_completed);
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
    let mut arg_lines = args.lines();
    let config_path = loop {
        let arg = arg_lines.next().expect("--mcp-config argument");
        if arg == "--mcp-config" {
            break std::path::PathBuf::from(arg_lines.next().expect("--mcp-config path argument"));
        }
    };
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&config_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    drop(stream);
    drop(backend);
    assert!(
        !config_path.exists(),
        "temporary MCP config was not removed"
    );
}

#[tokio::test]
async fn claude_adapter_routes_yolo_through_gate_without_tool_bridge() {
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
        let mut t = turn(tmp.path().to_path_buf(), None, BackendPermission::Yolo);
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

    // Approvals-only Yolo: Claude keeps its built-ins and trouve auto-allows
    // normal permission requests, while retaining the worktree path guard.
    // The read-only semantic search tools ride along pre-allowed.
    let args = std::fs::read_to_string(format!("{stub}.args")).unwrap();
    assert!(args.contains("--mcp-config"), "{args}");
    assert!(args.contains("--permission-prompt-tool"), "{args}");
    assert!(args.contains("mcp__trouve__approval_prompt"), "{args}");
    assert!(!args.contains("--dangerously-skip-permissions"), "{args}");
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

#[test]
fn backend_tool_free_capabilities_match_vendor_protocols() {
    let claude = ClaudeBackend::new("claude-code", Some("/nonexistent/claude".into()));
    let cursor = CursorBackend::new("cursor", Some("/nonexistent/cursor-agent".into()), None);
    let codex = CodexBackend::new("codex", Some("/nonexistent/codex".into()));

    assert!(claude.supports_tool_free_turns());
    assert!(!cursor.supports_tool_free_turns());
    assert!(!codex.supports_tool_free_turns());
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
