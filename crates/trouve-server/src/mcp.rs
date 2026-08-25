//! Streamable-HTTP MCP endpoint bridging external vendor agents (Claude
//! Code, Codex, Cursor) back into trouve — the successor to the old spawned
//! `mcp-bridge` subprocess.
//!
//! The engine points vendor agents at
//! `/internal/threads/{id}/mcp?tools=0|1&approval=0|1&ticket=...` as an HTTP MCP
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

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
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
    tools: Option<u8>,
    /// Serve the `approval_prompt` permission gate (Claude needs it; agents
    /// with native approval flows like Codex turn it off).
    approval: Option<u8>,
    /// Opaque active-turn capability issued by the engine. It binds this
    /// route path and both flags; the reusable process bridge token alone is
    /// intentionally insufficient authorization.
    ticket: Option<String>,
}

fn tool_call_is_available(name: &str, bridge_tools: bool, serve_approval: bool) -> bool {
    if name == "approval_prompt" {
        serve_approval
    } else {
        bridge_tools || ALWAYS_BRIDGED.contains(&name)
    }
}

pub(crate) async fn mcp_endpoint(
    State(engine): State<Arc<Engine>>,
    Path(thread_id): Path<String>,
    Query(q): Query<McpQuery>,
    Json(msg): Json<Value>,
) -> Response {
    let Some(bridge_tools) = q.tools.filter(|value| *value <= 1).map(|value| value == 1) else {
        return (
            StatusCode::UNAUTHORIZED,
            "missing or invalid bridge tools claim",
        )
            .into_response();
    };
    let Some(serve_approval) = q
        .approval
        .filter(|value| *value <= 1)
        .map(|value| value == 1)
    else {
        return (
            StatusCode::UNAUTHORIZED,
            "missing or invalid bridge approval claim",
        )
            .into_response();
    };
    let Some(ticket) = q.ticket.as_deref() else {
        return (StatusCode::UNAUTHORIZED, "missing bridge capability ticket").into_response();
    };
    let Some(claims) =
        engine.validate_bridge_ticket(ticket, &thread_id, bridge_tools, serve_approval)
    else {
        return (
            StatusCode::UNAUTHORIZED,
            "invalid or stale bridge capability ticket",
        )
            .into_response();
    };
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
        "tools/list" => tools_list(&engine, &thread_id, bridge_tools, serve_approval).await,
        "tools/call"
            if tool_call_is_available(
                msg["params"]["name"].as_str().unwrap_or_default(),
                bridge_tools,
                serve_approval,
            ) =>
        {
            if msg["params"]["name"] == "approval_prompt" {
                approval_prompt(&engine, &thread_id, &msg["params"]).await
            } else {
                tools_call(
                    &engine,
                    &thread_id,
                    &msg["params"],
                    claims.correlate_codex_owner,
                )
                .await
            }
        }
        "tools/call" => Err(format!(
            "tool is not available on this MCP bridge: {}",
            msg["params"]["name"].as_str().unwrap_or_default()
        )),
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
    match engine.bridged_tool_specs(thread_id, bridge_tools).await {
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

async fn tools_call(
    engine: &Arc<Engine>,
    thread_id: &str,
    params: &Value,
    correlate_codex_owner: bool,
) -> Result<Value, String> {
    let name = params["name"].as_str().unwrap_or_default();
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let result = if correlate_codex_owner {
        let (vendor_thread_id, vendor_call_id) = codex_tool_call_metadata(params)?;
        engine
            .bridged_codex_tool_call(
                thread_id,
                Some(vendor_thread_id),
                vendor_call_id,
                name,
                &arguments,
            )
            .await
            .map(|content| trouve_core::BridgedToolResult {
                content,
                images: Vec::new(),
            })
    } else {
        engine.bridged_tool_call(thread_id, name, &arguments).await
    };
    match result {
        Ok(result) => Ok(mcp_tool_success(result)),
        // Errors surface as tool results (isError) so the agent can react
        // instead of the whole turn failing.
        Err(e) => Ok(json!({
            "content": [ { "type": "text", "text": format!("tool call failed: {e}") } ],
            "isError": true,
        })),
    }
}

fn mcp_tool_success(result: trouve_core::BridgedToolResult) -> Value {
    let mut content = vec![json!({ "type": "text", "text": result.content })];
    content.extend(result.images.into_iter().map(|image| {
        json!({
            "type": "image",
            "data": image.data,
            "mimeType": image.mime,
        })
    }));
    let structured = serde_json::from_str::<Value>(&result.content)
        .ok()
        .filter(Value::is_object);
    let mut response = json!({
        "content": content,
        "isError": false,
    });
    if let Some(structured) = structured {
        response["structuredContent"] = structured;
    }
    response
}

fn codex_tool_call_metadata(params: &Value) -> Result<(&str, Option<&str>), String> {
    let metadata = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| "Codex MCP request is missing object _meta metadata".to_string())?;
    let thread_id = metadata
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|thread_id| !thread_id.is_empty())
        .ok_or_else(|| "Codex MCP request is missing string _meta.threadId".to_string())?;
    let call_id = match metadata.get("callId") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|call_id| !call_id.is_empty())
                .ok_or_else(|| {
                    "Codex MCP _meta.callId must be a non-empty string when present".to_string()
                })?,
        ),
    };
    Ok((thread_id, call_id))
}

#[cfg(test)]
mod tests {
    use super::{codex_tool_call_metadata, mcp_tool_success, tool_call_is_available};

    #[test]
    fn query_flags_gate_tool_execution_as_well_as_discovery() {
        assert!(tool_call_is_available("search", false, false));
        assert!(tool_call_is_available("find_related", false, false));
        assert!(tool_call_is_available("ask_question", false, false));
        assert!(!tool_call_is_available("write_file", false, false));
        assert!(!tool_call_is_available("approval_prompt", false, false));

        assert!(tool_call_is_available("write_file", true, false));
        assert!(!tool_call_is_available("approval_prompt", true, false));
        assert!(tool_call_is_available("approval_prompt", false, true));
    }

    #[test]
    fn codex_metadata_uses_only_explicit_thread_and_app_server_call_identity() {
        let params = serde_json::json!({
            "name": "search",
            "arguments": { "query": "same" },
            "_meta": { "threadId": "vendor-child", "callId": "item-42" },
            "id": "not-the-call-id",
            "toolCallId": "not-the-call-id-either"
        });
        assert_eq!(
            codex_tool_call_metadata(&params).unwrap(),
            ("vendor-child", Some("item-42"))
        );
        assert_eq!(
            codex_tool_call_metadata(&serde_json::json!({
                "_meta": { "threadId": "vendor-root" }
            }))
            .unwrap(),
            ("vendor-root", None)
        );
        for malformed in [
            serde_json::json!({}),
            serde_json::json!({ "_meta": {} }),
            serde_json::json!({ "_meta": { "threadId": "" } }),
            serde_json::json!({ "_meta": { "threadId": "vendor", "callId": 42 } }),
        ] {
            assert!(codex_tool_call_metadata(&malformed).is_err());
        }
    }

    #[test]
    fn bridged_tool_results_preserve_structured_and_image_content() {
        let result = mcp_tool_success(trouve_core::BridgedToolResult {
            content: r#"{"value":"ok"}"#.into(),
            images: vec![trouve_core::BridgedToolImage {
                mime: "image/png".into(),
                data: "aW1hZ2U=".into(),
            }],
        });
        assert_eq!(result["structuredContent"]["value"], "ok");
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["mimeType"], "image/png");
        assert_eq!(result["content"][1]["data"], "aW1hZ2U=");
    }
}
