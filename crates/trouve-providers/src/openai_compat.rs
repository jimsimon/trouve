//! OpenAI-compatible chat completions provider (API key auth).
//!
//! Works against api.openai.com and any compatible gateway (OpenRouter,
//! Ollama, vLLM, LiteLLM, ...) by overriding `base_url`.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::{json, Map, Value};
use trouve_protocol::Usage;

use crate::auth::{StaticToken, TokenSource};
use crate::{
    catalog, EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest,
    ToolSpec,
};

pub struct OpenAiCompatProvider {
    id: String,
    base_url: String,
    token: Arc<dyn TokenSource>,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::with_token(id, base_url, Arc::new(StaticToken(api_key.into())))
    }

    /// Any `TokenSource` — static key or refreshing OAuth tokens
    /// (subscription auth).
    pub fn with_token(
        id: impl Into<String>,
        base_url: impl Into<String>,
        token: Arc<dyn TokenSource>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
            client: reqwest::Client::new(),
        }
    }

    /// Standard OpenAI endpoint with the key from `OPENAI_API_KEY`.
    pub fn openai_from_env() -> Result<Self, ProviderError> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ProviderError::Auth("OPENAI_API_KEY is not set".into()))?;
        Ok(Self::new("openai", "https://api.openai.com/v1", key))
    }

    fn wire_messages(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|m| match m {
                Message::System(s) => json!({"role": "system", "content": s}),
                Message::User(s) => json!({"role": "user", "content": s}),
                Message::Assistant {
                    content,
                    tool_calls,
                } => {
                    let mut obj = json!({"role": "assistant", "content": content});
                    if !tool_calls.is_empty() {
                        obj["tool_calls"] = Value::Array(
                            tool_calls
                                .iter()
                                .map(|tc| {
                                    json!({
                                        "id": tc.id,
                                        "type": "function",
                                        "function": {
                                            "name": tc.name,
                                            "arguments": tc.arguments.to_string(),
                                        }
                                    })
                                })
                                .collect(),
                        );
                    }
                    obj
                }
                Message::ToolResult { call_id, content } => {
                    json!({"role": "tool", "tool_call_id": call_id, "content": content})
                }
            })
            .collect()
    }
}

/// Accumulates streamed tool-call fragments (OpenAI sends name/arguments in
/// pieces keyed by index).
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[async_trait::async_trait]
impl Provider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        // Only the canonical OpenAI endpoints get the static catalog;
        // arbitrary compatible gateways serve unknown model sets.
        if self.base_url.contains("api.openai.com") {
            catalog::openai_models(&self.id)
        } else {
            Vec::new()
        }
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Result<EventStream, ProviderError> {
        let mut body = json!({
            "model": model,
            "messages": Self::wire_messages(messages),
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(
                tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            );
        }
        for (k, v) in options {
            body[k.as_str()] = v.clone();
        }

        let key = self.token.bearer().await?;
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{status}: {text}")));
        }

        let byte_stream = resp.bytes_stream();
        let stream = async_stream(byte_stream);
        Ok(stream)
    }
}

/// Turn the SSE byte stream into `ProviderEvent`s.
fn async_stream(
    mut bytes: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
        + Send
        + Unpin
        + 'static,
) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);
    tokio::spawn(async move {
        let mut buf = String::new();
        let mut partials: Vec<PartialToolCall> = Vec::new();
        let mut usage = Usage::default();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(ProviderError::Request(e.to_string()))).await;
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // Process complete SSE lines; keep the remainder buffered.
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    for p in partials.drain(..) {
                        let arguments: Value =
                            serde_json::from_str(&p.arguments).unwrap_or(Value::Null);
                        let _ = tx
                            .send(Ok(ProviderEvent::ToolCall(ToolCallRequest {
                                id: p.id,
                                name: p.name,
                                arguments,
                            })))
                            .await;
                    }
                    let _ = tx
                        .send(Ok(ProviderEvent::Completed {
                            usage: usage.clone(),
                        }))
                        .await;
                    return;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                    usage.input_tokens = u["prompt_tokens"].as_u64().unwrap_or(0);
                    usage.output_tokens = u["completion_tokens"].as_u64().unwrap_or(0);
                    usage.cached_input_tokens = u
                        .pointer("/prompt_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                }
                let Some(delta) = v.pointer("/choices/0/delta") else {
                    continue;
                };
                if let Some(text) = delta.get("content").and_then(Value::as_str) {
                    if !text.is_empty() {
                        let _ = tx
                            .send(Ok(ProviderEvent::TextDelta(text.to_string())))
                            .await;
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let idx = call["index"].as_u64().unwrap_or(0) as usize;
                        while partials.len() <= idx {
                            partials.push(PartialToolCall::default());
                        }
                        let p = &mut partials[idx];
                        if let Some(id) = call.get("id").and_then(Value::as_str) {
                            p.id = id.to_string();
                        }
                        if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                            p.name.push_str(name);
                        }
                        if let Some(args) =
                            call.pointer("/function/arguments").and_then(Value::as_str)
                        {
                            p.arguments.push_str(args);
                        }
                    }
                }
            }
        }
        // Stream ended without [DONE]; still flush what we have.
        for p in partials.drain(..) {
            let arguments: Value = serde_json::from_str(&p.arguments).unwrap_or(Value::Null);
            let _ = tx
                .send(Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: p.id,
                    name: p.name,
                    arguments,
                })))
                .await;
        }
        let _ = tx.send(Ok(ProviderEvent::Completed { usage })).await;
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}
