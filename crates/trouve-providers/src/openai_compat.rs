//! OpenAI-compatible chat completions provider (API key auth).
//!
//! Works against api.openai.com and any compatible gateway (OpenRouter,
//! Ollama, vLLM, LiteLLM, ...) by overriding `base_url`.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::{Map, Value, json};
use trouve_protocol::Usage;

use crate::auth::{StaticToken, TokenSource};
use crate::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
    catalog,
};

pub struct OpenAiCompatProvider {
    id: String,
    base_url: String,
    token: Arc<dyn TokenSource>,
    client: reqwest::Client,
    /// Live `/models` result, cached for [`MODELS_TTL`].
    models_cache: tokio::sync::Mutex<Option<(std::time::Instant, Vec<trouve_protocol::ModelInfo>)>>,
}

const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

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
            models_cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Fetch the gateway's `/models` list (the OpenAI-standard endpoint,
    /// also served by OpenRouter-style gateways with richer metadata:
    /// display name, context length, pricing, tool support).
    async fn fetch_models(&self) -> Result<Vec<trouve_protocol::ModelInfo>, ProviderError> {
        let key = self.token.bearer().await?;
        let mut req = self.client.get(format!("{}/models", self.base_url));
        if !key.is_empty() {
            req = req.bearer_auth(key);
        }
        let resp = req
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
        Ok(parse_gateway_models(&self.id, &body))
    }

    /// Standard OpenAI endpoint with the key from `OPENAI_API_KEY`.
    pub fn openai_from_env() -> Result<Self, ProviderError> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ProviderError::Auth("OPENAI_API_KEY is not set".into()))?;
        Ok(Self::new("openai", "https://api.openai.com/v1", key))
    }

    fn wire_messages(messages: &[Message]) -> Vec<Value> {
        let mut wire = Vec::with_capacity(messages.len());
        for m in messages {
            match m {
                Message::System(s) => wire.push(json!({"role": "system", "content": s})),
                Message::User(s) => wire.push(json!({"role": "user", "content": s})),
                Message::Assistant {
                    content,
                    tool_calls,
                    // Anthropic-native reasoning blocks; not applicable here.
                    reasoning: _,
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
                    wire.push(obj);
                }
                Message::ToolResult {
                    call_id,
                    content,
                    images,
                } => {
                    wire.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
                    // The chat-completions tool role is text-only; images
                    // follow as a user message with image_url parts (the
                    // standard multimodal workaround).
                    if !images.is_empty() {
                        let mut parts = vec![json!({
                            "type": "text",
                            "text": format!("Image content from tool call {call_id}:"),
                        })];
                        parts.extend(images.iter().map(|img| {
                            json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", img.mime, img.data),
                                }
                            })
                        }));
                        wire.push(json!({"role": "user", "content": parts}));
                    }
                }
            }
        }
        wire
    }
}

/// Map a gateway `/models` response to ModelInfos. OpenRouter-style
/// entries carry display name, context length, pricing (USD per token, as
/// strings), and a `supported_parameters` capability list; plain
/// OpenAI-shaped gateways return bare ids, which get permissive defaults.
/// Models that declare no tool support are dropped — trouve's agent loop
/// needs tools.
fn parse_gateway_models(provider_id: &str, body: &Value) -> Vec<trouve_protocol::ModelInfo> {
    let Some(data) = body["data"].as_array() else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|entry| {
            let name = entry["id"].as_str()?;
            let window = entry["context_length"]
                .as_u64()
                .or_else(|| entry.pointer("/top_provider/context_length")?.as_u64())
                .filter(|w| *w > 0)
                .unwrap_or(128_000);
            let supports_tools = entry["supported_parameters"]
                .as_array()
                .map(|p| p.iter().any(|v| v.as_str() == Some("tools")))
                // No capability metadata: assume tools and let the gateway
                // reject if not.
                .unwrap_or(true);
            let price = |k: &str| -> Option<f64> {
                entry["pricing"][k]
                    .as_str()?
                    .parse::<f64>()
                    .ok()
                    .map(|v| v * 1e6)
            };
            supports_tools.then(|| trouve_protocol::ModelInfo {
                id: format!("{provider_id}/{name}"),
                display_name: entry["name"].as_str().unwrap_or(name).to_string(),
                context_window: window,
                supports_tools,
                input_price_per_mtok: price("prompt"),
                output_price_per_mtok: price("completion"),
                options_schema: serde_json::json!({}),
            })
        })
        .collect()
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

    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        // OpenAI proper: the static catalog carries curated options
        // schemas the live listing can't provide.
        if self.base_url.contains("api.openai.com") {
            return self.models();
        }
        let mut cache = self.models_cache.lock().await;
        if let Some((at, models)) = cache.as_ref()
            && at.elapsed() < MODELS_TTL
        {
            return models.clone();
        }
        match self.fetch_models().await {
            Ok(models) if !models.is_empty() => {
                *cache = Some((std::time::Instant::now(), models.clone()));
                models
            }
            Ok(_) => self.models(),
            Err(e) => {
                tracing::debug!("{} model list failed: {e}; using static catalog", self.id);
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
        let mut buf = crate::sse::LineBuffer::default();
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
            buf.push(&chunk);
            // Process complete SSE lines; keep the remainder buffered.
            while let Some(line) = buf.next_line() {
                let line = line.trim();
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
                if let Some(text) = delta.get("content").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    let _ = tx
                        .send(Ok(ProviderEvent::TextDelta(text.to_string())))
                        .await;
                }
                // DeepSeek-style reasoning stream on chat completions.
                if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    let _ = tx
                        .send(Ok(ProviderEvent::ThinkingDelta(text.to_string())))
                        .await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openrouter_style_gateway_models() {
        // Kilo Code / OpenRouter shape: rich metadata, string prices per
        // token, capability list.
        let body = json!({ "data": [
            {
                "id": "anthropic/claude-sonnet-4.5",
                "name": "Claude Sonnet 4.5",
                "context_length": 1_000_000,
                "pricing": { "prompt": "0.000003", "completion": "0.000015" },
                "supported_parameters": ["max_tokens", "tools", "temperature"],
            },
            {
                "id": "some/no-tools-model",
                "name": "No Tools",
                "context_length": 8192,
                "supported_parameters": ["max_tokens"],
            },
        ]});
        let models = parse_gateway_models("kilocode", &body);
        assert_eq!(models.len(), 1, "tool-less models are dropped");
        let m = &models[0];
        assert_eq!(m.id, "kilocode/anthropic/claude-sonnet-4.5");
        assert_eq!(m.display_name, "Claude Sonnet 4.5");
        assert_eq!(m.context_window, 1_000_000);
        assert_eq!(m.input_price_per_mtok, Some(3.0));
        assert_eq!(m.output_price_per_mtok, Some(15.0));
    }

    #[test]
    fn parses_bare_openai_shaped_model_list() {
        // Plain gateways (ollama, vllm) return ids only: permissive
        // defaults apply.
        let body = json!({ "data": [ { "id": "qwen2.5-coder:7b" } ] });
        let models = parse_gateway_models("ollama", &body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "ollama/qwen2.5-coder:7b");
        assert_eq!(models[0].context_window, 128_000);
        assert!(models[0].supports_tools);
        assert_eq!(models[0].input_price_per_mtok, None);
    }
}
