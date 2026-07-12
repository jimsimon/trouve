//! EXPERIMENTAL: direct client for ChatGPT's backend Codex Responses API.
//!
//! Uses the subscription token that `codex login` wrote to
//! `~/.codex/auth.json` to call the same endpoint the Codex CLI calls
//! (`https://chatgpt.com/backend-api/codex/responses`), keeping trouve's
//! native agent loop, tools, permissions, and compaction.
//!
//! Risk profile (surfaced in the UI as "Experimental"):
//! - The endpoint is undocumented and tolerated, not contracted. OpenAI can
//!   change or restrict it at any time; errors here should be expected.
//! - We deliberately never refresh the token ourselves — that would mean
//!   exercising the Codex CLI's OAuth client registration, the same
//!   "auth hijacking" pattern vendors close accounts for. When the token
//!   expires we ask the user to run any `codex` command, which refreshes
//!   `auth.json` through the sanctioned path.

use std::path::PathBuf;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::{EventStream, Message, Provider, ProviderError, ProviderEvent, ToolSpec};
use trouve_protocol::Usage;

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Base instructions sent as the Responses `instructions` field. The backend
/// expects a Codex-CLI-shaped system prompt; this is a trimmed rendition of
/// it. Override with `TROUVE_CODEX_INSTRUCTIONS_FILE` if OpenAI starts
/// validating the exact text (see openai/codex codex-rs/core/prompt.md).
const BASE_INSTRUCTIONS: &str = "You are Codex, based on GPT-5. You are running as a coding agent in the Codex CLI on a user's computer.\n\nGeneral guidance: solve the user's task by editing files and running commands in the workspace using the provided tools. Keep going until the task is fully resolved before yielding back to the user. Prefer making focused, minimal changes; follow the existing style of the codebase. Validate your work by running tests or builds where available.\n\nCommunication: be concise and direct. Summarize what you changed and why when you finish.";

pub struct CodexResponsesProvider {
    id: String,
    http: reqwest::Client,
    auth_path: PathBuf,
}

impl CodexResponsesProvider {
    pub fn new(id: &str) -> Self {
        let codex_home = std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".codex")
            });
        Self::with_auth_path(id, codex_home.join("auth.json"))
    }

    pub fn with_auth_path(id: &str, auth_path: PathBuf) -> Self {
        Self {
            id: id.to_string(),
            http: reqwest::Client::new(),
            auth_path,
        }
    }

    /// Read (access token, ChatGPT account id) from the Codex CLI's auth
    /// file, failing with actionable messages.
    fn credentials(&self) -> Result<(String, String), ProviderError> {
        let raw = std::fs::read_to_string(&self.auth_path).map_err(|_| {
            ProviderError::Auth(format!(
                "no Codex credentials at {} — run `codex login` first",
                self.auth_path.display()
            ))
        })?;
        let auth: Value = serde_json::from_str(&raw)
            .map_err(|e| ProviderError::Auth(format!("unreadable codex auth.json: {e}")))?;
        let tokens = &auth["tokens"];
        let access = tokens["access_token"]
            .as_str()
            .ok_or_else(|| ProviderError::Auth("codex auth.json has no access token".into()))?
            .to_string();

        if let Some(exp) = jwt_claim(&access, "exp").and_then(|v| v.as_u64()) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if exp <= now + 60 {
                // Deliberately not refreshed here; see module docs.
                return Err(ProviderError::Auth(
                    "Codex subscription token expired — run any `codex` command \
                     (e.g. `codex login status`) to refresh it"
                        .into(),
                ));
            }
        }

        let account_id = tokens["account_id"]
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                tokens["id_token"].as_str().and_then(|t| {
                    jwt_claim(t, "https://api.openai.com/auth")?["chatgpt_account_id"]
                        .as_str()
                        .map(str::to_string)
                })
            })
            .ok_or_else(|| {
                ProviderError::Auth("codex auth.json has no ChatGPT account id".into())
            })?;
        Ok((access, account_id))
    }
}

#[async_trait::async_trait]
impl Provider for CodexResponsesProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        crate::catalog::codex_models(&self.id)
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Result<EventStream, ProviderError> {
        let (access, account_id) = self.credentials()?;

        let instructions = std::env::var("TROUVE_CODEX_INSTRUCTIONS_FILE")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| BASE_INSTRUCTIONS.to_string());

        let mut body = json!({
            "model": model,
            "instructions": instructions,
            "input": input_items(messages),
            "tools": tools.iter().map(|t| json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "strict": false,
                "parameters": t.parameters,
            })).collect::<Vec<_>>(),
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "store": false,
            "stream": true,
            "include": [],
        });
        if let Some(effort) = options.get("reasoning_effort").and_then(Value::as_str) {
            body["reasoning"] = json!({ "effort": effort });
        }

        let url = std::env::var("TROUVE_CODEX_RESPONSES_URL")
            .unwrap_or_else(|_| CODEX_RESPONSES_URL.to_string());
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&access)
            .header("chatgpt-account-id", &account_id)
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("accept", "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let hint = if status.as_u16() == 401 || status.as_u16() == 403 {
                " (experimental endpoint: OpenAI may have rejected or restricted \
                 third-party access; try refreshing with `codex login`)"
            } else {
                ""
            };
            return Err(ProviderError::Api(format!("{status}: {text}{hint}")));
        }

        Ok(Box::pin(sse_events(resp)))
    }
}

/// Map trouve messages onto Responses API input items. The base system
/// prompt travels separately as `instructions`; trouve's own system message
/// (mode prompt, AGENTS.md) becomes a developer message.
fn input_items(messages: &[Message]) -> Vec<Value> {
    let mut items = Vec::with_capacity(messages.len());
    for m in messages {
        match m {
            Message::System(s) => items.push(json!({
                "type": "message",
                "role": "developer",
                "content": [{ "type": "input_text", "text": s }],
            })),
            Message::User(s) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": s }],
            })),
            Message::Assistant {
                content,
                tool_calls,
                reasoning: _,
            } => {
                if !content.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": content }],
                    }));
                }
                for call in tool_calls {
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments.to_string(),
                    }));
                }
            }
            Message::ToolResult {
                call_id,
                content,
                images,
            } => {
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content,
                }));
                // Function outputs are text-only in the Responses API;
                // images follow as a user message with input_image parts.
                if !images.is_empty() {
                    let mut parts = vec![json!({
                        "type": "input_text",
                        "text": format!("Image content from tool call {call_id}:"),
                    })];
                    parts.extend(images.iter().map(|img| {
                        json!({
                            "type": "input_image",
                            "image_url": format!("data:{};base64,{}", img.mime, img.data),
                        })
                    }));
                    items.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": parts,
                    }));
                }
            }
        }
    }
    items
}

/// Parse the Responses SSE stream into `ProviderEvent`s.
fn sse_events(
    resp: reqwest::Response,
) -> impl futures::Stream<Item = Result<ProviderEvent, ProviderError>> {
    use futures::StreamExt;
    let (tx, mut rx) = mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);
    tokio::spawn(async move {
        let mut bytes = resp.bytes_stream();
        let mut buf = crate::sse::LineBuffer::default();
        let mut usage = Usage::default();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(ProviderError::Request(e.to_string()))).await;
                    return;
                }
            };
            buf.push(&chunk);
            while let Some(line) = buf.next_line() {
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    return;
                }
                let Ok(ev) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                match ev["type"].as_str().unwrap_or("") {
                    "response.output_text.delta" => {
                        if let Some(d) = ev["delta"].as_str() {
                            let _ = tx.send(Ok(ProviderEvent::TextDelta(d.to_string()))).await;
                        }
                    }
                    // Reasoning summaries (and raw reasoning where exposed).
                    "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                        if let Some(d) = ev["delta"].as_str() {
                            let _ = tx
                                .send(Ok(ProviderEvent::ThinkingDelta(d.to_string())))
                                .await;
                        }
                    }
                    "response.output_item.done" => {
                        let item = &ev["item"];
                        if item["type"].as_str() == Some("function_call") {
                            let arguments = item["arguments"]
                                .as_str()
                                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                                .unwrap_or_else(|| item["arguments"].clone());
                            let _ = tx
                                .send(Ok(ProviderEvent::ToolCall(crate::ToolCallRequest {
                                    id: item["call_id"].as_str().unwrap_or("").to_string(),
                                    name: item["name"].as_str().unwrap_or("").to_string(),
                                    arguments,
                                })))
                                .await;
                        }
                    }
                    "response.completed" => {
                        let u = &ev["response"]["usage"];
                        usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
                        usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
                        usage.cached_input_tokens = u["input_tokens_details"]["cached_tokens"]
                            .as_u64()
                            .unwrap_or(0);
                        let _ = tx
                            .send(Ok(ProviderEvent::Completed {
                                usage: usage.clone(),
                            }))
                            .await;
                        return;
                    }
                    "response.failed" => {
                        let msg = ev["response"]["error"]["message"]
                            .as_str()
                            .unwrap_or("codex responses request failed")
                            .to_string();
                        let _ = tx.send(Err(ProviderError::Api(msg))).await;
                        return;
                    }
                    _ => {}
                }
            }
        }
    });
    futures::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// Decode one claim from a JWT payload without verifying the signature (we
/// only mine metadata the CLI wrote for ourselves).
fn jwt_claim(token: &str, claim: &str) -> Option<Value> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get(claim).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCallRequest;

    #[test]
    fn maps_messages_to_input_items() {
        let items = input_items(&[
            Message::System("mode prompt".into()),
            Message::User("hi".into()),
            Message::Assistant {
                content: "ok".into(),
                tool_calls: vec![ToolCallRequest {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "x"}),
                }],
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "c1".into(),
                content: "data".into(),
                images: vec![],
            },
        ]);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0]["role"], "developer");
        assert_eq!(items[1]["role"], "user");
        assert_eq!(items[2]["role"], "assistant");
        assert_eq!(items[3]["type"], "function_call");
        assert_eq!(items[3]["arguments"], "{\"path\":\"x\"}");
        assert_eq!(items[4]["type"], "function_call_output");
    }

    #[test]
    fn tool_result_images_follow_as_input_image() {
        let items = input_items(&[Message::ToolResult {
            call_id: "c1".into(),
            content: "image read".into(),
            images: vec![crate::ToolImage {
                mime: "image/png".into(),
                data: "QUJD".into(),
            }],
        }]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "function_call_output");
        assert_eq!(items[1]["role"], "user");
        assert_eq!(items[1]["content"][1]["type"], "input_image");
        assert_eq!(
            items[1]["content"][1]["image_url"],
            "data:image/png;base64,QUJD"
        );
    }

    #[test]
    fn jwt_claims_decode() {
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"exp":123,"https://api.openai.com/auth":{"chatgpt_account_id":"acct"}}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(jwt_claim(&token, "exp").unwrap(), json!(123));
        assert_eq!(
            jwt_claim(&token, "https://api.openai.com/auth").unwrap()["chatgpt_account_id"],
            "acct"
        );
    }

    #[test]
    fn missing_auth_file_is_actionable() {
        let p = CodexResponsesProvider::with_auth_path(
            "codex-api",
            PathBuf::from("/nonexistent/auth.json"),
        );
        let err = p.credentials().unwrap_err().to_string();
        assert!(err.contains("codex login"), "{err}");
    }
}
