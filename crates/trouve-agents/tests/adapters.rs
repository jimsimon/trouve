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
        attach_background: false,
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

/// Minimal HTTP stub for Cursor's API-key exchange and dashboard Connect-RPC
/// endpoints. It records each request's path and Authorization header.
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
                let body = if path == "/auth/exchange_user_api_key" {
                    r#"{"accessToken":"ephemeral-access-token"}"#
                } else if path.contains("GetCurrentPeriodUsage") {
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
async fn cursor_adapter_reads_dashboard_usage_without_cli_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    spawn_dashboard_stub(listener, seen.clone());

    // A deliberately nonexistent command proves the health query cannot
    // invoke Cursor's CLI. The configured API key is its only credential.
    let backend = CursorBackend::new(
        "cursor",
        Some("cursor-agent-must-not-run".into()),
        Some("cursor-user-api-key".into()),
    )
    .with_dashboard(base);
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

    // The configured key is sent only to the exchange endpoint. Dashboard
    // RPCs use the returned ephemeral token.
    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
    assert!(paths.contains(&"/auth/exchange_user_api_key"));
    assert!(paths.contains(&"/aiserver.v1.DashboardService/GetCurrentPeriodUsage"));
    assert!(paths.contains(&"/aiserver.v1.DashboardService/GetPlanInfo"));
    assert_eq!(
        seen.iter()
            .find(|(path, _)| path == "/auth/exchange_user_api_key")
            .map(|(_, auth)| auth.as_str()),
        Some("Bearer cursor-user-api-key")
    );
    assert!(
        seen.iter()
            .filter(|(path, _)| path.contains("DashboardService"))
            .all(|(_, auth)| auth == "Bearer ephemeral-access-token")
    );
}

#[tokio::test]
async fn cursor_health_requires_a_configured_api_key() {
    let backend = CursorBackend::new("cursor", Some("cursor-agent-must-not-run".into()), None);
    let health = backend.subscription_health().await.unwrap();
    assert_eq!(health.status, "unavailable");
    assert!(health.note.contains("API key"), "{}", health.note);
    assert!(!health.note.contains("cursor-agent"), "{}", health.note);
}

#[derive(Clone)]
struct CursorSdkMcpState {
    calls: std::sync::Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
}

async fn cursor_sdk_mcp(
    axum::extract::State(state): axum::extract::State<CursorSdkMcpState>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let id = request["id"].clone();
    let result = match request["method"].as_str().unwrap_or_default() {
        "tools/list" => serde_json::json!({
            "tools": [{
                "name": "trouve_test_echo",
                "description": "Return the test sentinel.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "token": { "type": "string" } },
                    "required": ["token"],
                    "additionalProperties": false
                }
            }]
        }),
        "tools/call" => {
            let params = request["params"].clone();
            let expected = serde_json::json!({
                "name": "trouve_test_echo",
                "arguments": { "token": "from-sdk" }
            });
            if params != expected {
                return axum::Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32602,
                        "message": format!("unexpected tools/call params: {params}")
                    }
                }));
            }
            state.calls.lock().await.push(params);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            serde_json::json!({
                "content": [{ "type": "text", "text": "tool-ok" }],
                "structuredContent": { "value": "tool-ok" },
                "isError": false
            })
        }
        other => {
            return axum::Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unsupported {other}") }
            }));
        }
    };
    axum::Json(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn cursor_sdk_bridge_stub(dir: &Path) -> String {
    write_stub(
        dir,
        "cursor-sdk-bridge",
        r##"#!/usr/bin/env python3
import http.client
import http.server
import json
import os
import socketserver
import struct
import sys
import threading
import time
import urllib.parse

binary = os.path.abspath(sys.argv[0])
with open(binary + ".pid", "w", encoding="utf-8") as destination:
    destination.write(str(os.getpid()))
count_path = binary + ".spawns"
try:
    with open(count_path, "r", encoding="utf-8") as source:
        count = int(source.read().strip()) + 1
except Exception:
    count = 1
with open(count_path, "w", encoding="utf-8") as destination:
    destination.write(str(count))
agent_id = "sdk-agent-1"
bridge_token = "test-bridge-token"
callback_url = os.environ["CURSOR_SDK_TOOL_CALLBACK_URL"]
callback_token = os.environ["CURSOR_SDK_TOOL_CALLBACK_AUTH_TOKEN"]
callback_history = []
active_options = {}
send_count = 0
callback_updates = 0
cancel_received = threading.Event()

expected_custom_tool = {
    "description": "Return the test sentinel.",
    "inputSchema": {
        "type": "object",
        "properties": {"token": {"type": "string"}},
        "required": ["token"],
        "additionalProperties": False
    }
}

def valid_tool_registration(options):
    names = options.get("tools", {}).get("names", [])
    tools = options.get("local", {}).get("customTools", {})
    if names == ["mcp"]:
        return tools == {"trouve_test_echo": expected_custom_tool}
    return names == [] and tools == {}

def write_json(path, value):
    with open(path, "w", encoding="utf-8") as destination:
        json.dump(value, destination)

def frame(value, flags=0):
    payload = json.dumps(value, separators=(",", ":")).encode("utf-8")
    return struct.pack(">BI", flags, len(payload)) + payload

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        global active_options, callback_history, callback_token, callback_updates, callback_url, send_count
        if self.headers.get("Authorization") != "Bearer " + bridge_token:
            self.send_response(401)
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        if self.path.endswith("/Send"):
            if len(raw) < 5:
                self.send_response(400)
                self.end_headers()
                return
            request = json.loads(raw[5:].decode("utf-8"))
            write_json(binary + ".send.json", request)
            send_count += 1
            if "STALL_FOR_CANCELLATION" in request.get("message", {}).get("text", ""):
                run_id = "sdk-run-cancel"
                first = frame({
                    "sdkMessage": {
                        "type": "assistant",
                        "message": {
                            "type": "assistant",
                            "message": {
                                "content": [{"type": "text", "text": "RUN_READY"}]
                            }
                        }
                    }
                })
                self.protocol_version = "HTTP/1.1"
                self.send_response(200)
                self.send_header("Content-Type", "application/connect+json")
                self.send_header("Transfer-Encoding", "chunked")
                self.end_headers()
                self.wfile.write(format(len(first), "x").encode("ascii") + b"\r\n")
                self.wfile.write(first + b"\r\n")
                self.wfile.flush()
                deadline = time.monotonic() + 10
                while not os.path.exists(binary + ".release-run-id"):
                    if time.monotonic() >= deadline:
                        self.wfile.write(b"0\r\n\r\n")
                        self.wfile.flush()
                        return
                    time.sleep(0.01)
                identity = frame({
                    "sdkMessage": {
                        "type": "system",
                        "message": {
                            "type": "system",
                            "agent_id": agent_id,
                            "run_id": run_id
                        }
                    }
                })
                self.wfile.write(format(len(identity), "x").encode("ascii") + b"\r\n")
                self.wfile.write(identity + b"\r\n")
                self.wfile.flush()
                if not cancel_received.wait(10):
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                    return
                cancelled = "RUN_LIFECYCLE_STATUS_CANCELLED"
                messages = [
                    {
                        "result": {
                            "agentId": agent_id,
                            "runId": run_id,
                            "status": cancelled,
                            "result": {
                                "agentId": agent_id,
                                "runId": run_id,
                                "status": cancelled
                            }
                        }
                    },
                    {"done": {"agentId": agent_id, "runId": run_id}}
                ]
                body = b"".join(frame(message) for message in messages) + frame({}, 2)
                self.wfile.write(format(len(body), "x").encode("ascii") + b"\r\n")
                self.wfile.write(body + b"\r\n0\r\n\r\n")
                self.wfile.flush()
                return
            names = active_options.get("tools", {}).get("names", [])
            if names == ["mcp"]:
                import concurrent.futures
                tool_name = "trouve_test_echo"
                callback = {
                    "toolName": tool_name,
                    "toolCallId": "sdk-call-1",
                    "agentId": agent_id,
                    "args": {"token": "from-sdk"}
                }
                def call_tool(_attempt):
                    # The callback is always adapter-owned loopback traffic.
                    # Bypass urllib's macOS system-proxy discovery so bridge
                    # startup never depends on platform proxy initialization.
                    callback_parts = urllib.parse.urlsplit(callback_url)
                    connection = http.client.HTTPConnection(
                        callback_parts.hostname, callback_parts.port, timeout=10
                    )
                    try:
                        connection.request(
                            "POST",
                            "/sdk.v1.SdkCustomToolCallbackService/CallCustomTool",
                            body=json.dumps(callback).encode("utf-8"),
                            headers={
                            "Authorization": "Bearer " + callback_token,
                            "Content-Type": "application/json"
                            }
                        )
                        response = connection.getresponse()
                        if response.status != 200:
                            raise RuntimeError("callback returned " + str(response.status))
                        return json.loads(response.read().decode("utf-8"))
                    finally:
                        connection.close()
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                    callback_responses = list(executor.map(call_tool, range(2)))
                write_json(binary + ".callback.json", callback_responses[0])
                write_json(binary + ".callback-replay.json", callback_responses[1])
            run_id = "sdk-run-" + str(send_count)
            finished = "RUN_LIFECYCLE_STATUS_FINISHED"
            messages = [
                {
                    "sdkMessage": {
                        "type": "system",
                        "message": {
                            "type": "system",
                            "agent_id": agent_id,
                            "run_id": run_id
                        }
                    }
                },
                {
                    "sdkMessage": {
                        "type": "assistant",
                        "message": {
                            "type": "assistant",
                            "message": {
                                "content": [{"type": "text", "text": "SDK done"}]
                            }
                        }
                    }
                },
                {
                    "result": {
                        "agentId": agent_id,
                        "runId": run_id,
                        "status": finished,
                        "result": {
                            "agentId": agent_id,
                            "runId": run_id,
                            "status": finished,
                            "result": "SDK done",
                            "usage": {
                                "inputTokens": "7",
                                "outputTokens": "3",
                                "cacheReadTokens": "2"
                            }
                        }
                    }
                },
                {"done": {"agentId": agent_id, "runId": run_id}}
            ]
            body = b"".join(frame(message) for message in messages) + frame({}, 2)
            self.send_response(200)
            self.send_header("Content-Type", "application/connect+json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        request = json.loads(raw.decode("utf-8") or "{}")
        status = 200
        if self.path.endswith("/CreateAgent"):
            write_json(binary + ".create.json", request)
            active_options = request.get("options", {})
            if valid_tool_registration(active_options):
                response = {"agentId": agent_id}
            else:
                status = 400
                response = {"code": "invalid_argument", "message": "invalid custom tool registration"}
        elif self.path.endswith("/ResumeAgent"):
            write_json(binary + ".resume.json", request)
            active_options = request.get("options", {})
            if valid_tool_registration(active_options):
                response = {"agentId": agent_id}
            else:
                status = 400
                response = {"code": "invalid_argument", "message": "invalid custom tool registration"}
        elif self.path.endswith("/SetToolCallback"):
            callback_url = request.get("url", "")
            callback_token = request.get("authToken", "")
            callback_history.append({"url": callback_url, "authToken": callback_token})
            write_json(binary + ".callback-history.json", callback_history)
            callback_updates += 1
            with open(binary + ".callback-updates", "w", encoding="utf-8") as destination:
                destination.write(str(callback_updates))
            response = {}
        elif self.path.endswith("/CloseAgent"):
            response = {}
        elif self.path.endswith("/CancelRun"):
            with open(binary + ".cancel-run", "w", encoding="utf-8") as destination:
                destination.write(request.get("runId", ""))
            cancel_received.set()
            response = {}
        elif self.path.endswith("/Shutdown"):
            with open(binary + ".shutdown", "w", encoding="utf-8") as destination:
                destination.write("shutdown")
            response = {}
            threading.Thread(target=self.server.shutdown, daemon=True).start()
        else:
            response = {}
        body = json.dumps(response, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

class LoopbackHTTPServer(http.server.ThreadingHTTPServer):
    def server_bind(self):
        # HTTPServer resolves its bind address through getfqdn() while setting
        # display metadata. GitHub's macOS runner can stall in that unrelated
        # resolver path, so preserve TCPServer binding and use the literal
        # loopback address as the never-displayed server name.
        socketserver.TCPServer.server_bind(self)
        self.server_name = self.server_address[0]
        self.server_port = self.server_address[1]

server = LoopbackHTTPServer(("127.0.0.1", 0), Handler)
with open(binary + ".port", "w", encoding="utf-8") as destination:
    destination.write(str(server.server_address[1]))
ready = {
    "schemaVersion": 1,
    "transport": "tcp",
    "protocol": "connect",
    "url": "http://127.0.0.1:" + str(server.server_address[1]),
    "authToken": bridge_token
}
print("cursor-sdk-bridge ready " + json.dumps(ready), file=sys.stderr, flush=True)
server.serve_forever()
"##,
    )
}

#[tokio::test]
async fn cursor_adapter_uses_sdk_bridge_and_trouve_owned_tools() {
    use axum::routing::post;

    let tmp = tempfile::tempdir().unwrap();
    let calls = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server_calls = calls.clone();
    let mcp_task = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new()
                .route("/mcp", post(cursor_sdk_mcp))
                .with_state(CursorSdkMcpState {
                    calls: server_calls,
                }),
        )
        .await
    });
    let stub = cursor_sdk_bridge_stub(tmp.path());
    let backend = CursorBackend::new(
        "cursor",
        Some(stub.clone()),
        Some("test-cursor-api-key".into()),
    )
    .with_state_root(tmp.path().join("sdk-state"));
    let mut first_turn = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
    first_turn.mcp_bridge = Some(trouve_agents::McpBridgeConfig {
        url: format!("http://{address}/mcp"),
        bridge_tools: true,
        disallowed_tools: Vec::new(),
    });
    let mut stream = start_turn(&backend, || {
        let mut next = turn(tmp.path().to_path_buf(), None, BackendPermission::Ask);
        next.mcp_bridge = first_turn.mcp_bridge.clone();
        next
    })
    .await;

    let events = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }
        events
    })
    .await
    .expect("initial Cursor SDK stream did not close within five seconds");
    assert!(events.iter().any(
        |event| matches!(event, BackendEvent::SessionStarted { session_id } if session_id == "sdk-agent-1")
    ), "{events:?}");
    assert!(
        events.iter().all(|event| !matches!(
            event,
            BackendEvent::ToolStarted { .. } | BackendEvent::ToolCompleted { .. }
        )),
        "the internal MCP endpoint persists the canonical tool lifecycle; the adapter must not duplicate it"
    );
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            BackendEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "SDK done");
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::Completed { usage }
            if usage.input_tokens == 7
                && usage.output_tokens == 3
                && usage.cached_input_tokens == 2
    )));
    let create: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.create.json")).unwrap())
            .unwrap();
    assert_eq!(create["options"]["apiKey"], "test-cursor-api-key");
    assert_eq!(
        create["options"]["tools"]["names"],
        serde_json::json!(["mcp"])
    );
    assert_eq!(
        create["options"]["disallowedTools"],
        serde_json::json!([
            "shell",
            "read",
            "edit",
            "grep",
            "glob",
            "ls",
            "task",
            "webSearch",
            "delete",
            "readLints",
            "webFetch",
            "semSearch",
            "updateTodos",
            "readTodos",
            "askQuestion",
            "await",
            "generateImage",
            "applyAgentDiff"
        ])
    );
    assert_eq!(
        create["options"]["local"]["settingSources"],
        serde_json::json!([])
    );
    assert_eq!(
        create["options"]["local"]["sandboxOptions"]["enabled"],
        false
    );
    assert_eq!(
        create["options"]["local"]["customTools"]["trouve_test_echo"],
        serde_json::json!({
            "description": "Return the test sentinel.",
            "inputSchema": {
                "type": "object",
                "properties": { "token": { "type": "string" } },
                "required": ["token"],
                "additionalProperties": false
            }
        })
    );
    let send: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.send.json")).unwrap())
            .unwrap();
    assert!(
        send["message"]["text"]
            .as_str()
            .unwrap()
            .contains("<mode-instructions>")
    );
    assert_eq!(send["options"]["mode"], "AGENT_MODE_OPTION_AGENT");
    assert_eq!(calls.lock().await.len(), 1);
    let callback: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.callback.json")).unwrap())
            .unwrap();
    assert_eq!(callback["result"]["structuredContent"]["value"], "tool-ok");
    let replay: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{stub}.callback-replay.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        replay, callback,
        "a retried callback must replay its result"
    );

    let mut resumed = start_turn(&backend, || {
        let mut next = turn(
            tmp.path().to_path_buf(),
            Some("sdk-agent-1"),
            BackendPermission::ReadOnly,
        );
        next.tool_free = true;
        next
    })
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = resumed.next().await {
            event.unwrap();
        }
    })
    .await
    .expect("resumed Cursor SDK stream did not close within five seconds");
    let resume: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{stub}.resume.json")).unwrap())
            .unwrap();
    assert_eq!(resume["agentId"], "sdk-agent-1");
    assert_eq!(resume["options"]["tools"]["names"], serde_json::json!([]));
    assert_eq!(resume["options"]["mode"], "AGENT_MODE_OPTION_PLAN");
    assert_eq!(
        std::fs::read_to_string(format!("{stub}.spawns"))
            .unwrap()
            .trim(),
        "1",
        "two turns on one Trouve thread should reuse one Bridge process"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{stub}.callback-updates"))
            .unwrap()
            .trim(),
        "4",
        "each turn should register and then clear its callback"
    );
    let callback_history: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{stub}.callback-history.json")).unwrap(),
    )
    .unwrap();
    let callback_history = callback_history.as_array().unwrap();
    assert_eq!(callback_history.len(), 4);
    assert!(!callback_history[0]["url"].as_str().unwrap().is_empty());
    assert!(
        !callback_history[0]["authToken"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        callback_history[1],
        serde_json::json!({ "url": "", "authToken": "" })
    );
    assert!(!callback_history[2]["url"].as_str().unwrap().is_empty());
    assert_ne!(
        callback_history[0]["authToken"], callback_history[2]["authToken"],
        "a reused Bridge must receive a fresh callback bearer for each turn"
    );
    assert_eq!(
        callback_history[3],
        serde_json::json!({ "url": "", "authToken": "" })
    );
    assert_eq!(calls.lock().await.len(), 1);
    backend.shutdown().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(format!("{stub}.shutdown")).unwrap(),
        "shutdown",
        "backend shutdown must gracefully stop its retained warm Bridge"
    );
    mcp_task.abort();
}

#[tokio::test]
async fn cursor_adapter_cancellation_acknowledges_cancel_run_and_reaps_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = cursor_sdk_bridge_stub(tmp.path());
    let backend = CursorBackend::new(
        "cursor",
        Some(stub.clone()),
        Some("test-cursor-api-key".into()),
    )
    .with_state_root(tmp.path().join("sdk-state"));
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = start_turn(&backend, || {
        let mut next = turn(tmp.path().to_path_buf(), None, BackendPermission::ReadOnly);
        next.prompt = "STALL_FOR_CANCELLATION".into();
        next.tool_free = true;
        next.cancel = cancel.clone();
        next
    })
    .await;

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match stream.next().await {
                Some(Ok(BackendEvent::TextDelta(text))) if text == "RUN_READY" => {
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("Cursor Send failed before cancellation: {error}"),
                None => panic!("Cursor Send ended before publishing cancellation readiness"),
            }
        }
    })
    .await
    .expect("Cursor Send did not publish cancellation readiness");
    #[cfg(target_os = "linux")]
    let pid: u32 = std::fs::read_to_string(format!("{stub}.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    cancel.cancel();
    std::fs::write(format!("{stub}.release-run-id"), "").unwrap();

    let mut saw_cancelled = false;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while let Some(event) = stream.next().await {
            if matches!(event, Err(trouve_agents::BackendError::Cancelled)) {
                saw_cancelled = true;
            }
        }
    })
    .await
    .expect("cancelled Cursor turn did not finish bounded cleanup");
    assert!(saw_cancelled);
    assert_eq!(
        std::fs::read_to_string(format!("{stub}.cancel-run"))
            .unwrap()
            .trim(),
        "sdk-run-cancel"
    );
    let port: u16 = std::fs::read_to_string(format!("{stub}.port"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "cancelled Cursor Bridge was still accepting connections"
    );
    #[cfg(target_os = "linux")]
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "cancelled Cursor stream closed before the Bridge process was reaped"
    );
}

#[tokio::test]
async fn cursor_backend_shutdown_drains_an_active_send_before_reaping_bridge() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = cursor_sdk_bridge_stub(tmp.path());
    let backend = std::sync::Arc::new(
        CursorBackend::new(
            "cursor",
            Some(stub.clone()),
            Some("test-cursor-api-key".into()),
        )
        .with_state_root(tmp.path().join("sdk-state")),
    );
    let mut next = turn(tmp.path().to_path_buf(), None, BackendPermission::ReadOnly);
    next.prompt = "STALL_FOR_CANCELLATION".into();
    next.tool_free = true;
    let mut stream = backend.run_turn(next).await.unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match stream.next().await {
                Some(Ok(BackendEvent::TextDelta(text))) if text == "RUN_READY" => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("Cursor Send failed before shutdown: {error}"),
                None => panic!("Cursor Send ended before publishing shutdown readiness"),
            }
        }
    })
    .await
    .expect("Cursor Send did not publish shutdown readiness");

    let shutting_backend = backend.clone();
    let shutdown_started = std::time::Instant::now();
    let mut shutdown = tokio::spawn(async move { shutting_backend.shutdown().await });
    let mut saw_shutdown = false;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = stream.next().await {
            if let Err(error) = event {
                saw_shutdown |= error.to_string().contains("pool is shutting down");
            }
        }
    })
    .await
    .expect("active Cursor turn did not stop after the bounded shutdown drain");
    assert!(saw_shutdown, "active turn did not report pool shutdown");
    tokio::time::timeout(std::time::Duration::from_secs(5), &mut shutdown)
        .await
        .expect("backend shutdown remained blocked behind the active turn")
        .unwrap()
        .unwrap();
    assert!(shutdown_started.elapsed() >= std::time::Duration::from_millis(750));

    let port: u16 = std::fs::read_to_string(format!("{stub}.port"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "shut down Cursor Bridge was still accepting connections"
    );
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
IFS= read -r unsubscribe
echo '{"jsonrpc":"2.0","id":5,"result":{}}'
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
IFS= read -r unsubscribe
printf '%s\n' "$unsubscribe" > "$0.unsubscribe.tmp"
mv "$0.unsubscribe.tmp" "$0.unsubscribe"
echo '{"jsonrpc":"2.0","id":5,"result":{}}'
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

    let unsubscribe_path = std::path::PathBuf::from(format!("{stub}.unsubscribe"));
    tokio::time::timeout(deadline, async {
        while !unsubscribe_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("stream-drop cleanup must release the vendor thread");
    let unsubscribe: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(unsubscribe_path).unwrap()).unwrap();
    assert_eq!(unsubscribe["method"], "thread/unsubscribe");
    assert_eq!(unsubscribe["params"]["threadId"], "thr-1");
}

#[tokio::test]
async fn codex_adapter_releases_completed_thread_when_queued_terminal_races_stream_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-terminal-drop",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # turn/start
# Queue completion before acknowledging turn/start so it is already routed
# when the exposed stream is returned to the caller.
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
IFS= read -r cleanup
printf '%s\n' "$cleanup" > "$0.cleanup.tmp"
mv "$0.cleanup.tmp" "$0.cleanup"
if [[ "$cleanup" == *'"method":"thread/unsubscribe"'* ]]; then
    echo '{"jsonrpc":"2.0","id":4,"result":{}}'
elif [[ "$cleanup" == *'"method":"turn/interrupt"'* ]]; then
    echo '{"jsonrpc":"2.0","id":4,"result":{}}'
fi
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
    let cleanup_path = std::path::PathBuf::from(format!("{stub}.cleanup"));
    tokio::time::timeout(deadline, async {
        while !cleanup_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("queued terminal cleanup must reach the app-server");

    let cleanup: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cleanup_path).unwrap()).unwrap();
    assert_eq!(cleanup["method"], "thread/unsubscribe");
    assert_eq!(cleanup["params"]["threadId"], "thr-1");
}

#[tokio::test]
async fn codex_adapter_delivers_completion_before_unsubscribe_is_acknowledged() {
    let tmp = tempfile::tempdir().unwrap();
    let stub = write_stub(
        tmp.path(),
        "codex-slow-unsubscribe",
        r#"#!/bin/bash
IFS= read -r line # initialize
echo '{"jsonrpc":"2.0","id":1,"result":{}}'
IFS= read -r line # initialized notification
IFS= read -r line # thread/start
echo '{"jsonrpc":"2.0","id":2,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # turn/start
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
echo '{"jsonrpc":"2.0","id":3,"result":{"turn":{"id":"turn-1"}}}'
IFS= read -r unsubscribe
printf '%s\n' "$unsubscribe" > "$0.unsubscribe.tmp"
mv "$0.unsubscribe.tmp" "$0.unsubscribe"
# Deliberately withhold the response: terminal delivery must not wait for it,
# but replacement setup must remain behind this cleanup boundary.
while [[ ! -f "$0.release" ]]; do sleep 0.01; done
echo '{"jsonrpc":"2.0","id":4,"result":{}}'
IFS= read -r resume
printf '%s\n' "$resume" > "$0.follow-up.tmp"
mv "$0.follow-up.tmp" "$0.follow-up"
echo '{"jsonrpc":"2.0","id":5,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r turn_start
echo '{"jsonrpc":"2.0","id":6,"result":{"turn":{"id":"turn-2"}}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-2","status":"completed"}}}'
IFS= read -r final_unsubscribe
echo '{"jsonrpc":"2.0","id":7,"result":{}}'
cat > /dev/null
"#,
    );
    let backend = std::sync::Arc::new(CodexBackend::new("codex", Some(stub.clone())));
    let mut stream = start_turn(backend.as_ref(), || {
        turn(tmp.path().to_path_buf(), None, BackendPermission::Ask)
    })
    .await;

    tokio::time::timeout(std::time::Duration::from_millis(500), async {
        loop {
            match stream.next().await {
                Some(Ok(BackendEvent::Completed { .. })) => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("turn failed before completion: {error}"),
                None => panic!("stream ended before completion"),
            }
        }
    })
    .await
    .expect("terminal delivery waited for thread/unsubscribe");

    let unsubscribe_path = std::path::PathBuf::from(format!("{stub}.unsubscribe"));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !unsubscribe_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("terminal cleanup did not send thread/unsubscribe");
    let unsubscribe: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(unsubscribe_path).unwrap()).unwrap();
    assert_eq!(unsubscribe["method"], "thread/unsubscribe");

    let replacement = tokio::spawn({
        let backend = std::sync::Arc::clone(&backend);
        let worktree = tmp.path().to_path_buf();
        async move {
            let mut replacement = turn(worktree, Some("thr-1"), BackendPermission::Ask);
            replacement.prompt = "follow-up".into();
            backend.run_turn(replacement).await
        }
    });
    let follow_up_path = std::path::PathBuf::from(format!("{stub}.follow-up"));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !follow_up_path.exists(),
        "replacement setup overtook the pending thread/unsubscribe"
    );

    std::fs::write(format!("{stub}.release"), "").unwrap();
    let mut replacement_stream =
        tokio::time::timeout(std::time::Duration::from_secs(2), replacement)
            .await
            .expect("replacement setup remained blocked after unsubscribe acknowledgement")
            .expect("replacement task panicked")
            .expect("replacement setup failed");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match replacement_stream.next().await {
                Some(Ok(BackendEvent::Completed { .. })) => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("replacement turn failed: {error}"),
                None => panic!("replacement stream ended before completion"),
            }
        }
    })
    .await
    .expect("replacement turn did not complete");
    let follow_up: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(follow_up_path).unwrap()).unwrap();
    assert_eq!(follow_up["method"], "thread/resume");
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
IFS= read -r unsubscribe
echo '{"jsonrpc":"2.0","id":5,"result":{}}'
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
IFS= read -r line # first thread/unsubscribe
echo '{"jsonrpc":"2.0","id":5,"result":{}}'
IFS= read -r line # replacement thread/resume after release
printf '%s\n' "$line" > "$0.thread-resume"
echo '{"jsonrpc":"2.0","id":6,"result":{"thread":{"id":"thr-1"}}}'
IFS= read -r line # replacement turn/start
echo '{"jsonrpc":"2.0","id":7,"result":{"turn":{"id":"turn-2"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-1","delta":"stale"}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}'
echo '{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-2","delta":"replacement"}}'
echo '{"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-2","status":"completed"}}}'
IFS= read -r line # replacement thread/unsubscribe
echo '{"jsonrpc":"2.0","id":8,"result":{}}'
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
        std::path::Path::new(&format!("{stub}.thread-resume")).exists(),
        "a released cancelled thread must be resumed before reuse"
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
    let cursor = CursorBackend::new(
        "cursor",
        Some("/nonexistent/cursor-sdk-bridge".into()),
        None,
    );
    let codex = CodexBackend::new("codex", Some("/nonexistent/codex".into()));

    assert!(claude.supports_tool_free_turns());
    assert!(cursor.supports_tool_free_turns());
    assert!(!codex.supports_tool_free_turns());
    assert!(!claude.confines_read_only_turns());
    assert!(cursor.confines_read_only_turns());
    assert!(!codex.confines_read_only_turns());
}

#[tokio::test]
async fn status_reports_missing_binary() {
    let backend = ClaudeBackend::new("claude-code", Some("/nonexistent/claude".into()));
    assert!(!backend.status().installed);
    let backend = CursorBackend::new(
        "cursor",
        Some("/nonexistent/cursor-sdk-bridge".into()),
        None,
    );
    assert!(!backend.status().installed);
    let backend = CodexBackend::new("codex", Some("/nonexistent/codex".into()));
    assert!(!backend.status().installed);
}
