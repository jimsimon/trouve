//! Streamable-HTTP MCP endpoint bridging external vendor agents (Claude,
//! Codex, and Cursor) back into trouve — the successor to the old spawned
//! `mcp-bridge` subprocess.
//!
//! The engine points vendor agents at
//! `/internal/threads/{id}/mcp?approval=0|1&ticket=...` as an HTTP MCP server.
//! It serves Trouve-owned supplemental capabilities and user-configured MCP
//! tools; with `approval=1` it also serves
//! `approval_prompt` (Claude's `--permission-prompt-tool` target: permission
//! requests become trouve approvals). Every `tools/call` goes straight into
//! the engine, so supplemental calls flow through the same permission gate,
//! approval hub, and event log as native provider calls.
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

/// Trouve-owned capabilities that supplement the vendor's optimized core
/// tools. User MCP tools are included dynamically by their `mcp__` prefix.
const SUPPLEMENTAL_TOOLS: &[&str] = &[
    "search",
    "find_related",
    "ask_question",
    "load_skill",
    "todo_write",
    "search_transcript",
    "spawn_thread",
    "spawn_session",
    "spawn_output",
];

#[derive(serde::Deserialize)]
pub(crate) struct McpQuery {
    /// Serve the `approval_prompt` permission gate (Claude needs it; agents
    /// with native approval flows like Codex turn it off).
    approval: Option<u8>,
    /// Opaque active-turn capability issued by the engine. It binds this
    /// route path and approval flag; the reusable process bridge token alone is
    /// intentionally insufficient authorization.
    ticket: Option<String>,
    /// Revision embedded in the vendor's configured URL. The engine prepared
    /// this catalog before launching the turn, so tools/list can reuse it.
    #[serde(default)]
    catalog_revision: Option<String>,
}

fn tool_available_for_bridge(name: &str, serve_approval: bool) -> bool {
    if name == "approval_prompt" {
        serve_approval
    } else {
        SUPPLEMENTAL_TOOLS.contains(&name) || name.starts_with("mcp__")
    }
}

pub(crate) async fn mcp_endpoint(
    State(engine): State<Arc<Engine>>,
    Path(thread_id): Path<String>,
    Query(q): Query<McpQuery>,
    Json(msg): Json<Value>,
) -> Response {
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
    let Some(claims) = engine.validate_bridge_ticket(ticket, &thread_id, serve_approval) else {
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
        "tools/list" => {
            let revision = q
                .catalog_revision
                .as_deref()
                .and_then(|value| u64::from_str_radix(value, 16).ok());
            tools_list(
                &engine,
                &thread_id,
                serve_approval,
                revision,
                claims.builtin_skills_enabled,
            )
            .await
        }
        "tools/call"
            if tool_available_for_bridge(
                msg["params"]["name"].as_str().unwrap_or_default(),
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
                    claims.builtin_skills_enabled,
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
    serve_approval: bool,
    catalog_revision: Option<u64>,
    builtin_skills_enabled: bool,
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
    let specs = match engine
        .bridged_tool_specs_for_revision_with_skills(
            thread_id,
            catalog_revision,
            builtin_skills_enabled,
        )
        .await
    {
        Ok(specs) => specs,
        Err(error) if serve_approval => {
            // Claude's permission-prompt hook is independently available and
            // must keep working when supplemental MCP discovery is degraded.
            tracing::warn!(
                thread_id,
                "supplemental tool catalog unavailable; serving approval fallback: {error}"
            );
            return Ok(json!({ "tools": tools }));
        }
        Err(error) => {
            return Err(format!(
                "supplemental tool catalog unavailable for {thread_id}: {error}"
            ));
        }
    };
    tools.extend(
        specs
            .iter()
            .filter(|s| tool_available_for_bridge(&s.name, serve_approval))
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "inputSchema": s.parameters,
                })
            }),
    );
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
    builtin_skills_enabled: bool,
) -> Result<Value, String> {
    let name = params["name"].as_str().unwrap_or_default();
    if !tool_available_for_bridge(name, false) {
        return Err(format!("tool {name:?} is disabled for this bridge"));
    }
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
    let result = if correlate_codex_owner {
        let (vendor_thread_id, vendor_call_id) = codex_tool_call_metadata(params)?;
        engine
            .bridged_codex_tool_call_with_skills(
                thread_id,
                Some(vendor_thread_id),
                vendor_call_id,
                name,
                &arguments,
                builtin_skills_enabled,
            )
            .await
    } else {
        engine
            .bridged_tool_call_with_skills(thread_id, name, &arguments, builtin_skills_enabled)
            .await
    };
    match result {
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
    use super::{codex_tool_call_metadata, tool_available_for_bridge, tools_list};
    use trouve_core::{Engine, config::Config, store::Store};

    #[test]
    fn supplemental_catalog_gates_tool_execution() {
        assert!(tool_available_for_bridge("search", false));
        assert!(tool_available_for_bridge("find_related", false));
        assert!(tool_available_for_bridge("ask_question", false));
        assert!(tool_available_for_bridge("load_skill", false));
        assert!(tool_available_for_bridge("mcp__jira__search", false));
        assert!(!tool_available_for_bridge("write_file", false));
        assert!(!tool_available_for_bridge("approval_prompt", false));
        assert!(tool_available_for_bridge("approval_prompt", true));
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

    #[tokio::test]
    async fn approval_tool_survives_supplemental_catalog_failure() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config::default(),
        );

        let response = tools_list(&engine, "missing-thread", true, None, true)
            .await
            .expect("approval fallback should remain independently available");
        let tools = response["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "approval_prompt");

        assert!(
            tools_list(&engine, "missing-thread", false, None, true)
                .await
                .is_err(),
            "catalog-only bridges must still surface discovery failures"
        );
    }
}
