//! `trouve mcp-bridge` — a minimal stdio MCP server bridging an external
//! vendor agent (Claude Code) back into trouve.
//!
//! The engine launches vendor agents with `--mcp-config` pointing at this
//! process and `TROUVE_SERVER` / `TROUVE_THREAD_ID` in its environment. It
//! always serves `approval_prompt` (Claude's `--permission-prompt-tool`
//! target: permission requests become trouve approvals) plus trouve's
//! read-only semantic search tools, which complement the vendor's built-ins.
//! With `TROUVE_BRIDGE_TOOLS=1` it additionally serves the full ToolExecutor
//! tool set; every `tools/call` dials back into the engine's internal
//! endpoints, so bridged calls flow through the same permission gate,
//! approval hub, and event log as native tool calls.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

pub async fn run() -> Result<()> {
    let server = std::env::var("TROUVE_SERVER").context("TROUVE_SERVER env var not set")?;
    let thread_id =
        std::env::var("TROUVE_THREAD_ID").context("TROUVE_THREAD_ID env var not set")?;
    let bridge_tools = std::env::var("TROUVE_BRIDGE_TOOLS").is_ok_and(|v| v == "1");
    // Claude needs the approval-prompt gate; agents with native approval
    // flows (Codex) set this to "0" so the tool isn't even listed.
    let serve_approval = std::env::var("TROUVE_BRIDGE_APPROVAL").map_or(true, |v| v != "0");
    let base = format!(
        "{}/internal/threads/{}",
        server.trim_end_matches('/'),
        thread_id
    );
    let http = reqwest::Client::new();

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = stdin.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = msg["method"].as_str().unwrap_or("");
        let id = msg["id"].clone();
        if id.is_null() {
            // Notification (e.g. notifications/initialized): nothing to say.
            continue;
        }
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": msg["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or(MCP_PROTOCOL_VERSION),
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "trouve-bridge",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "Prefer the `search` tool over grep/file scans when \
                    exploring the codebase: it is a pre-built hybrid semantic index and \
                    returns file paths with exact line numbers. Use `find_related` with a \
                    result's file_path and line to discover similar code.",
            })),
            "ping" => Ok(json!({})),
            "tools/list" => tools_list(&http, &base, bridge_tools, serve_approval).await,
            "tools/call" if msg["params"]["name"] == "approval_prompt" => {
                approval_prompt(&http, &base, &msg["params"]).await
            }
            "tools/call" => tools_call(&http, &base, &msg["params"]).await,
            _ => Err(format!("method not supported: {method}")),
        };
        let response = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": message },
            }),
        };
        let mut out = serde_json::to_vec(&response)?;
        out.push(b'\n');
        stdout.write_all(&out).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// Tools served even without full tool bridging: the vendor agent keeps its
/// own built-ins, but trouve's native semantic search and the interactive
/// question tool (harness features the vendor has no equivalent of) are
/// always offered.
const ALWAYS_BRIDGED: &[&str] = &["search", "find_related", "ask_question"];

async fn tools_list(
    http: &reqwest::Client,
    base: &str,
    bridge_tools: bool,
    serve_approval: bool,
) -> Result<Value, String> {
    // The approval gate is served for Claude (its permission-prompt tool is
    // invoked by name and must exist on the configured MCP server).
    let mut tools = Vec::new();
    if serve_approval {
        tools.push(json!({
            "name": "approval_prompt",
            "description": "Permission gate: asks the trouve user to approve a tool call. \
                            Invoked automatically by the harness, not meant to be called directly.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string" },
                    "input": { "type": "object" },
                    "tool_use_id": { "type": "string" },
                },
                "required": ["tool_name", "input"],
            },
        }));
    }
    // Best-effort: the approval gate must exist even when the engine is
    // briefly unreachable, so a failed spec fetch just serves fewer tools.
    let specs = fetch_tool_specs(http, base).await.unwrap_or_else(|e| {
        eprintln!("trouve-bridge: tool specs unavailable: {e}");
        Value::Null
    });
    if let Some(specs) = specs.as_array() {
        tools.extend(
            specs
                .iter()
                .filter(|s| {
                    bridge_tools
                        || s["name"]
                            .as_str()
                            .is_some_and(|n| ALWAYS_BRIDGED.contains(&n))
                })
                .map(|s| {
                    json!({
                        "name": s["name"],
                        "description": s["description"],
                        "inputSchema": s["parameters"],
                    })
                }),
        );
    }
    Ok(json!({ "tools": tools }))
}

async fn fetch_tool_specs(http: &reqwest::Client, base: &str) -> Result<Value, String> {
    http.get(format!("{base}/tools"))
        .send()
        .await
        .map_err(|e| format!("engine unreachable: {e}"))?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// Relay one Claude permission request to the engine's approval flow and
/// answer in the shape `--permission-prompt-tool` expects: a JSON-encoded
/// `{"behavior": "allow"|"deny", ...}` payload in the text content.
async fn approval_prompt(
    http: &reqwest::Client,
    base: &str,
    params: &Value,
) -> Result<Value, String> {
    let args = &params["arguments"];
    let tool = args["tool_name"].as_str().unwrap_or("tool");
    let input = args.get("input").cloned().unwrap_or(json!({}));
    let approved = match http
        .post(format!("{base}/approval"))
        .json(&json!({ "tool": tool, "args": input }))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["approved"].as_bool())
            .unwrap_or(false),
        // Fail closed: an unreachable engine means no approval.
        _ => false,
    };
    let verdict = if approved {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": "denied by the trouve user" })
    };
    Ok(json!({
        "content": [ { "type": "text", "text": verdict.to_string() } ],
    }))
}

async fn tools_call(http: &reqwest::Client, base: &str, params: &Value) -> Result<Value, String> {
    let name = params["name"].as_str().unwrap_or_default();
    let body = json!({
        "name": name,
        "arguments": params.get("arguments").cloned().unwrap_or(json!({})),
    });
    let resp = http
        .post(format!("{base}/tools/call"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("engine unreachable: {e}"))?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        // Errors surface as tool results (isError) so the agent can react
        // instead of the whole turn failing.
        return Ok(json!({
            "content": [ { "type": "text", "text": format!("tool call failed: {text}") } ],
            "isError": true,
        }));
    }
    let out: Value = resp.json().await.map_err(|e| e.to_string())?;
    let content = out["content"].as_str().unwrap_or_default().to_string();
    Ok(json!({
        "content": [ { "type": "text", "text": content } ],
        "isError": false,
    }))
}
