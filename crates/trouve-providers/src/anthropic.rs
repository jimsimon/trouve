//! Anthropic Messages API provider (streaming).

use futures::StreamExt;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use trouve_protocol::Usage;

use crate::auth::TokenSource;
use crate::{
    catalog, EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest,
    ToolSpec,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u64 = 8192;

pub struct AnthropicProvider {
    id: String,
    base_url: String,
    token: Arc<dyn TokenSource>,
    client: reqwest::Client,
    /// Subscription (OAuth) tokens authenticate with `Authorization: Bearer`
    /// plus the oauth beta header instead of `x-api-key`.
    oauth_bearer: bool,
    /// Live `/v1/models` result, cached for [`MODELS_TTL`].
    models_cache: tokio::sync::Mutex<Option<(std::time::Instant, Vec<trouve_protocol::ModelInfo>)>>,
}

/// How long a fetched model list stays fresh.
const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

impl AnthropicProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: Option<String>,
        token: Arc<dyn TokenSource>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url
                .unwrap_or_else(|| "https://api.anthropic.com".into())
                .trim_end_matches('/')
                .to_string(),
            token,
            client: reqwest::Client::new(),
            oauth_bearer: false,
            models_cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Add auth headers (API key or OAuth bearer) to a request.
    fn authed(&self, req: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
        let req = req.header("anthropic-version", ANTHROPIC_VERSION);
        if self.oauth_bearer {
            req.bearer_auth(key)
                .header("anthropic-beta", "oauth-2025-04-20")
        } else {
            req.header("x-api-key", key)
        }
    }

    /// Fetch the account's model list from `/v1/models`, keeping the static
    /// catalog's pricing where ids match (the API doesn't report pricing).
    async fn fetch_models(&self) -> Result<Vec<trouve_protocol::ModelInfo>, ProviderError> {
        let key = self.token.bearer().await?;
        let resp = self
            .authed(
                self.client
                    .get(format!("{}/v1/models?limit=100", self.base_url)),
                &key,
            )
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ProviderError::Api(format!("{}", resp.status())));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        let known = catalog::anthropic_models(&self.id);
        let Some(data) = body["data"].as_array() else {
            return Ok(Vec::new());
        };
        Ok(data
            .iter()
            .filter_map(|entry| {
                let name = entry["id"].as_str()?;
                let known = known.iter().find(|k| k.id.ends_with(&format!("/{name}")));
                let window = entry["max_input_tokens"]
                    .as_u64()
                    .filter(|w| *w > 0)
                    .or(known.map(|k| k.context_window))
                    .unwrap_or(200_000);
                let thinking = entry
                    .pointer("/capabilities/thinking/supported")
                    .and_then(Value::as_bool)
                    // Older API versions omit capabilities; assume thinking.
                    .unwrap_or(true);
                Some(trouve_protocol::ModelInfo {
                    id: format!("{}/{name}", self.id),
                    display_name: entry["display_name"].as_str().unwrap_or(name).to_string(),
                    context_window: window,
                    supports_tools: true,
                    input_price_per_mtok: known.and_then(|k| k.input_price_per_mtok),
                    output_price_per_mtok: known.and_then(|k| k.output_price_per_mtok),
                    options_schema: if thinking {
                        catalog::anthropic_thinking_schema()
                    } else {
                        serde_json::json!({"type": "object", "properties": {}})
                    },
                    max_mode: false,
                })
            })
            .collect())
    }

    pub fn with_oauth_bearer(mut self) -> Self {
        self.oauth_bearer = true;
        self
    }

    /// Split provider-agnostic messages into Anthropic's system string plus
    /// user/assistant turns with content blocks.
    fn wire_messages(messages: &[Message]) -> (String, Vec<Value>) {
        let mut system = String::new();
        let mut wire: Vec<Value> = Vec::new();
        for m in messages {
            match m {
                Message::System(s) => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(s);
                }
                Message::User(s) => wire.push(json!({"role": "user", "content": s})),
                Message::Assistant {
                    content,
                    tool_calls,
                } => {
                    let mut blocks = Vec::new();
                    if !content.is_empty() {
                        blocks.push(json!({"type": "text", "text": content}));
                    }
                    for tc in tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.arguments,
                        }));
                    }
                    if blocks.is_empty() {
                        blocks.push(json!({"type": "text", "text": ""}));
                    }
                    wire.push(json!({"role": "assistant", "content": blocks}));
                }
                Message::ToolResult { call_id, content } => {
                    // Tool results are user-role content blocks; consecutive
                    // results merge into the previous user message if it is
                    // already a block list.
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": content,
                    });
                    if let Some(last) = wire.last_mut() {
                        if last["role"] == "user" && last["content"].is_array() {
                            last["content"].as_array_mut().unwrap().push(block);
                            continue;
                        }
                    }
                    wire.push(json!({"role": "user", "content": [block]}));
                }
            }
        }
        (system, wire)
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        catalog::anthropic_models(&self.id)
    }

    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        let mut cache = self.models_cache.lock().await;
        if let Some((at, models)) = cache.as_ref() {
            if at.elapsed() < MODELS_TTL {
                return models.clone();
            }
        }
        match self.fetch_models().await {
            Ok(models) if !models.is_empty() => {
                *cache = Some((std::time::Instant::now(), models.clone()));
                models
            }
            Ok(_) => self.models(),
            Err(e) => {
                tracing::debug!("anthropic model list failed: {e}; using static catalog");
                self.models()
            }
        }
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Result<EventStream, ProviderError> {
        let (system, wire) = Self::wire_messages(messages);
        let mut body = json!({
            "model": model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "messages": wire,
            "stream": true,
        });
        if !system.is_empty() {
            body["system"] = system.into();
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(
                tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.parameters,
                        })
                    })
                    .collect(),
            );
        }
        for (k, v) in options {
            // Map trouve's generic option names onto Anthropic's shapes.
            match k.as_str() {
                "thinking_level" => {
                    if let Some(budget) = v.as_str().and_then(catalog::thinking_budget_tokens) {
                        body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
                    }
                }
                "thinking_budget_tokens" => {
                    body["thinking"] = json!({"type": "enabled", "budget_tokens": v});
                }
                _ => body[k.as_str()] = v.clone(),
            }
        }
        // API constraints with thinking enabled: max_tokens must exceed the
        // budget, and temperature must stay at its default.
        if let Some(budget) = body
            .pointer("/thinking/budget_tokens")
            .and_then(Value::as_u64)
        {
            let max = body["max_tokens"].as_u64().unwrap_or(DEFAULT_MAX_TOKENS);
            if max <= budget {
                body["max_tokens"] = json!(budget + DEFAULT_MAX_TOKENS);
            }
            body.as_object_mut().unwrap().remove("temperature");
        }

        let key = self.token.bearer().await?;
        let req = self.authed(
            self.client.post(format!("{}/v1/messages", self.base_url)),
            &key,
        );
        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{status}: {text}")));
        }

        Ok(sse_to_events(resp.bytes_stream()))
    }
}

#[derive(Default)]
struct BlockState {
    tool_id: String,
    tool_name: String,
    tool_json: String,
    is_tool: bool,
}

fn sse_to_events(
    mut bytes: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
        + Send
        + Unpin
        + 'static,
) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);
    tokio::spawn(async move {
        let mut buf = String::new();
        let mut usage = Usage::default();
        let mut blocks: std::collections::HashMap<u64, BlockState> = Default::default();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(ProviderError::Request(e.to_string()))).await;
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                match v["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        usage.input_tokens = v
                            .pointer("/message/usage/input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        usage.cached_input_tokens = v
                            .pointer("/message/usage/cache_read_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    }
                    "content_block_start" => {
                        let idx = v["index"].as_u64().unwrap_or(0);
                        let block = blocks.entry(idx).or_default();
                        if v.pointer("/content_block/type").and_then(Value::as_str)
                            == Some("tool_use")
                        {
                            block.is_tool = true;
                            block.tool_id = v
                                .pointer("/content_block/id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            block.tool_name = v
                                .pointer("/content_block/name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                    "content_block_delta" => {
                        let idx = v["index"].as_u64().unwrap_or(0);
                        match v.pointer("/delta/type").and_then(Value::as_str) {
                            Some("text_delta") => {
                                if let Some(text) = v.pointer("/delta/text").and_then(Value::as_str)
                                {
                                    let _ = tx
                                        .send(Ok(ProviderEvent::TextDelta(text.to_string())))
                                        .await;
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(text) =
                                    v.pointer("/delta/thinking").and_then(Value::as_str)
                                {
                                    let _ = tx
                                        .send(Ok(ProviderEvent::ThinkingDelta(text.to_string())))
                                        .await;
                                }
                            }
                            Some("input_json_delta") => {
                                if let Some(part) =
                                    v.pointer("/delta/partial_json").and_then(Value::as_str)
                                {
                                    blocks.entry(idx).or_default().tool_json.push_str(part);
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        let idx = v["index"].as_u64().unwrap_or(0);
                        if let Some(block) = blocks.remove(&idx) {
                            if block.is_tool {
                                let arguments: Value = if block.tool_json.is_empty() {
                                    json!({})
                                } else {
                                    serde_json::from_str(&block.tool_json).unwrap_or(Value::Null)
                                };
                                let _ = tx
                                    .send(Ok(ProviderEvent::ToolCall(ToolCallRequest {
                                        id: block.tool_id,
                                        name: block.tool_name,
                                        arguments,
                                    })))
                                    .await;
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(out) = v.pointer("/usage/output_tokens").and_then(Value::as_u64)
                        {
                            usage.output_tokens = out;
                        }
                    }
                    "message_stop" => {
                        let _ = tx
                            .send(Ok(ProviderEvent::Completed {
                                usage: usage.clone(),
                            }))
                            .await;
                        return;
                    }
                    "error" => {
                        let _ = tx
                            .send(Err(ProviderError::Api(v["error"].to_string())))
                            .await;
                        return;
                    }
                    _ => {}
                }
            }
        }
        let _ = tx.send(Ok(ProviderEvent::Completed { usage })).await;
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_merge_into_one_user_message() {
        let messages = vec![
            Message::System("sys".into()),
            Message::User("hi".into()),
            Message::Assistant {
                content: "calling".into(),
                tool_calls: vec![
                    ToolCallRequest {
                        id: "t1".into(),
                        name: "a".into(),
                        arguments: json!({}),
                    },
                    ToolCallRequest {
                        id: "t2".into(),
                        name: "b".into(),
                        arguments: json!({}),
                    },
                ],
            },
            Message::ToolResult {
                call_id: "t1".into(),
                content: "r1".into(),
            },
            Message::ToolResult {
                call_id: "t2".into(),
                content: "r2".into(),
            },
        ];
        let (system, wire) = AnthropicProvider::wire_messages(&messages);
        assert_eq!(system, "sys");
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[2]["role"], "user");
        assert_eq!(wire[2]["content"].as_array().unwrap().len(), 2);
    }
}
