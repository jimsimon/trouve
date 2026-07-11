//! Streamable-HTTP MCP endpoint bridging external vendor agents (Claude
//! Code, Codex) back into trouve — the successor to the old spawned
//! `mcp-bridge` subprocess.
//!
//! The engine points vendor agents at
//! `/internal/threads/{id}/mcp?tools=0|1&approval=0|1` as an HTTP MCP
//! server. It always serves trouve's read-only semantic search tools and
//! the interactive question tool; with `approval=1` it serves
//! `approval_prompt` (Claude's `--permission-prompt-tool` target: permission
//! requests become trouve approvals); with `tools=1` it additionally serves
//! the full ToolExecutor tool set. Every `tools/call` goes straight into
//! the engine, so bridged calls flow through the same permission gate,
//! approval hub, and event log as native tool calls.
//!
//! Stateless per the MCP streamable-HTTP transport: plain JSON responses
//! (no SSE upgrade, no session ids), notifications get `202 Accepted`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use trouve_core::Engine;

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// Tools served even without full tool bridging: the vendor agent keeps its
/// own built-ins, but trouve's native semantic search and the interactive
/// question tool (harness features the vendor has no equivalent of) are
/// always offered.
const ALWAYS_BRIDGED: &[&str] = &["search", "find_related", "ask_question"];

#[derive(serde::Deserialize)]
pub(crate) struct McpQuery {
    /// Serve the full ToolExecutor tool set (vendor built-ins stand down).
    #[serde(default)]
    tools: u8,
    /// Serve the `approval_prompt` permission gate (Claude needs it; agents
    /// with native approval flows like Codex turn it off).
    #[serde(default = "default_approval")]
    approval: u8,
}

fn default_approval() -> u8 {
    1
}

pub(crate) async fn mcp_endpoint(
    State(engine): State<Arc<Engine>>,
    Path(thread_id): Path<String>,
    Query(q): Query<McpQuery>,
    Json(msg): Json<Value>,
) -> Response {
    let method = msg["method"].as_str().unwrap_or("");
    let id = msg["id"].clone();
    if id.is_null() {
        // Notification (e.g. notifications/initialized): nothing to say.
        return StatusCode::ACCEPTED.into_response();
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
        "tools/list" => tools_list(&engine, &thread_id, q.tools == 1, q.approval != 0).await,
        "tools/call" if msg["params"]["name"] == "approval_prompt" => {
            approval_prompt(&engine, &thread_id, &msg["params"]).await
        }
        "tools/call" => tools_call(&engine, &thread_id, &msg["params"]).await,
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
    Json(response).into_response()
}

async fn tools_list(
    engine: &Engine,
    thread_id: &str,
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
    // Best-effort: the approval gate must exist even when the thread lookup
    // fails, so a failed spec fetch just serves fewer tools.
    match engine.bridged_tool_specs(thread_id).await {
        Ok(specs) => tools.extend(
            specs
                .iter()
                .filter(|s| bridge_tools || ALWAYS_BRIDGED.contains(&s.name.as_str()))
                .map(|s| {
                    json!({
                        "name": s.name,
                        "description": s.description,
                        "inputSchema": s.parameters,
                    })
                }),
        ),
        Err(e) => tracing::warn!("mcp bridge: tool specs unavailable for {thread_id}: {e}"),
    }
    Ok(json!({ "tools": tools }))
}

/// Relay one Claude permission request to the engine's approval flow and
/// answer in the shape `--permission-prompt-tool` expects: a JSON-encoded
/// `{"behavior": "allow"|"deny", ...}` payload in the text content.
async fn approval_prompt(
    engine: &Engine,
    thread_id: &str,
    params: &Value,
) -> Result<Value, String> {
    let args = &params["arguments"];
    let tool = args["tool_name"].as_str().unwrap_or("tool");
    let input = args.get("input").cloned().unwrap_or(json!({}));
    // Fail closed: an engine error means no approval.
    let approved = engine
        .bridged_approval(thread_id, tool, &input)
        .await
        .unwrap_or(false);
    let verdict = if approved {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": "denied by the trouve user" })
    };
    Ok(json!({
        "content": [ { "type": "text", "text": verdict.to_string() } ],
    }))
}

async fn tools_call(engine: &Engine, thread_id: &str, params: &Value) -> Result<Value, String> {
    let name = params["name"].as_str().unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    match engine.bridged_tool_call(thread_id, name, &arguments).await {
        Ok(content) => Ok(json!({
            "content": [ { "type": "text", "text": content } ],
            "isError": false,
        })),
        // Errors surface as tool results (isError) so the agent can react
        // instead of the whole turn failing.
        Err(e) => Ok(json!({
            "content": [ { "type": "text", "text": format!("tool call failed: {e}") } ],
            "isError": true,
        })),
    }
}
