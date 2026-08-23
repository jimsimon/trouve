//! OpenAI-compatible chat completions provider (API key auth).
//!
//! Works against api.openai.com and any compatible gateway (OpenRouter,
//! Ollama, vLLM, LiteLLM, ...) by overriding `base_url`.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use serde_json::{Map, Value, json};
use trouve_protocol::Usage;

use crate::auth::{StaticToken, TokenSource};
use crate::models_dev::{ModelsDevCatalog, OptionsDialect};
use crate::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
};

pub struct OpenAiCompatProvider {
    id: String,
    base_url: String,
    token: Arc<dyn TokenSource>,
    client: reqwest::Client,
    catalog: Arc<ModelsDevCatalog>,
    catalog_provider: Option<String>,
    bearer_auth: bool,
    headers: BTreeMap<String, String>,
    query_params: BTreeMap<String, String>,
    /// Live `/models` result, cached for [`MODELS_TTL`].
    models_cache: tokio::sync::Mutex<Option<(std::time::Instant, Vec<trouve_protocol::ModelInfo>)>>,
}

const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);
#[cfg(not(test))]
const MODELS_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(test)]
const MODELS_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Clone, Copy)]
enum NativeModelApi {
    Ollama,
    LmStudio,
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
            catalog: Arc::new(ModelsDevCatalog::embedded()),
            catalog_provider: None,
            bearer_auth: true,
            headers: BTreeMap::new(),
            query_params: BTreeMap::new(),
            models_cache: tokio::sync::Mutex::new(None),
        }
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelsDevCatalog>) -> Self {
        self.catalog = catalog;
        self
    }

    /// Pin catalog metadata when an endpoint template was expanded at setup
    /// time and therefore cannot be matched literally against models.dev.
    pub fn with_catalog_provider(mut self, provider: impl Into<String>) -> Self {
        self.catalog_provider = Some(provider.into());
        self
    }

    /// Configure non-standard but still OpenAI-compatible request auth.
    pub fn with_http_options(
        mut self,
        bearer_auth: bool,
        headers: BTreeMap<String, String>,
        query_params: BTreeMap<String, String>,
    ) -> Self {
        self.bearer_auth = bearer_auth;
        self.headers = headers;
        self.query_params = query_params;
        self
    }

    fn authed(&self, mut request: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
        if self.bearer_auth && !key.is_empty() {
            request = request.bearer_auth(key);
        }
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if !self.query_params.is_empty() {
            request = request.query(&self.query_params);
        }
        request
    }

    /// Fetch the gateway's `/models` list. Catalog-covered providers use it
    /// only as an account-availability overlay; custom gateways and local
    /// runtimes retain their explicit live-metadata adapters.
    async fn fetch_models(&self) -> Result<Vec<trouve_protocol::ModelInfo>, ProviderError> {
        let key = self.token.bearer().await?;
        let req = self.authed(self.client.get(format!("{}/models", self.base_url)), &key);
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
        if !body["data"].is_array() {
            return Err(ProviderError::Api(
                "models response is missing its data array".into(),
            ));
        }
        let catalog_provider = self.catalog_provider_id();
        let models = if let Some(catalog_provider) = catalog_provider {
            parse_catalog_models(&self.id, &body, &catalog_provider, &self.catalog)
        } else {
            parse_gateway_models(&self.id, &body)
        };
        Ok(self.enrich_from_native_api(models, &key).await)
    }

    fn catalog_provider_id(&self) -> Option<String> {
        self.catalog_provider.clone().or_else(|| {
            self.catalog
                .provider_for_endpoint(&self.id, &self.base_url, "openai-compat")
        })
    }

    /// Rebuild catalog-covered cache entries from their available ids so a
    /// models.dev refresh is visible immediately even while the live
    /// availability TTL remains valid.
    fn canonicalize_models(
        &self,
        models: Vec<trouve_protocol::ModelInfo>,
    ) -> Vec<trouve_protocol::ModelInfo> {
        let Some(catalog_provider) = self.catalog_provider_id() else {
            return models;
        };
        let prefix = format!("{}/", self.id);
        self.catalog.provider_models_for_ids(
            &catalog_provider,
            &self.id,
            models
                .iter()
                .map(|model| model.id.strip_prefix(&prefix).unwrap_or(model.id.as_str())),
            OptionsDialect::OpenAi,
        )
    }

    /// OpenAI-compatible endpoints intentionally expose only a lowest-common-
    /// denominator model list. Recognized local runtimes have richer native
    /// endpoints that need no separate credential or model registry.
    fn native_model_api(&self) -> Option<NativeModelApi> {
        let url = reqwest::Url::parse(&self.base_url).ok()?;
        let loopback = matches!(
            url.host_str()?.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "::1"
        );
        match self.id.as_str() {
            "ollama" => Some(NativeModelApi::Ollama),
            "lmstudio" | "lm-studio" => Some(NativeModelApi::LmStudio),
            _ if loopback && url.port_or_known_default() == Some(11434) => {
                Some(NativeModelApi::Ollama)
            }
            _ if loopback && url.port_or_known_default() == Some(1234) => {
                Some(NativeModelApi::LmStudio)
            }
            _ => None,
        }
    }

    fn native_url(&self, path: &str) -> Option<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.base_url).ok()?;
        url.set_path(path);
        url.set_query(None);
        url.set_fragment(None);
        Some(url)
    }

    async fn enrich_from_native_api(
        &self,
        models: Vec<trouve_protocol::ModelInfo>,
        key: &str,
    ) -> Vec<trouve_protocol::ModelInfo> {
        match self.native_model_api() {
            Some(NativeModelApi::Ollama) => self.enrich_from_ollama(models, key).await,
            Some(NativeModelApi::LmStudio) => self.enrich_from_lm_studio(models, key).await,
            None => models,
        }
    }

    async fn enrich_from_ollama(
        &self,
        models: Vec<trouve_protocol::ModelInfo>,
        key: &str,
    ) -> Vec<trouve_protocol::ModelInfo> {
        let Some(url) = self.native_url("/api/show") else {
            return models;
        };
        let provider_prefix = format!("{}/", self.id);
        futures::stream::iter(models.into_iter().map(|mut model| {
            let client = self.client.clone();
            let url = url.clone();
            let key = key.to_string();
            let native_id = model
                .id
                .strip_prefix(&provider_prefix)
                .unwrap_or(&model.id)
                .to_string();
            async move {
                let mut request = client.post(url).json(&json!({"model": native_id}));
                if !key.is_empty() {
                    request = request.bearer_auth(key);
                }
                if let Ok(response) = request.send().await
                    && let Ok(response) = response.error_for_status()
                    && let Ok(body) = response.json::<Value>().await
                {
                    apply_ollama_metadata(&mut model, &body);
                }
                model
            }
        }))
        .buffered(4)
        .filter(|model| std::future::ready(model.supports_tools))
        .collect()
        .await
    }

    async fn enrich_from_lm_studio(
        &self,
        mut models: Vec<trouve_protocol::ModelInfo>,
        key: &str,
    ) -> Vec<trouve_protocol::ModelInfo> {
        let Some(url) = self.native_url("/api/v1/models") else {
            return models;
        };
        let mut request = self.client.get(url);
        if !key.is_empty() {
            request = request.bearer_auth(key);
        }
        if let Ok(response) = request.send().await
            && let Ok(response) = response.error_for_status()
            && let Ok(body) = response.json::<Value>().await
        {
            apply_lm_studio_metadata(&mut models, &self.id, &body);
        }
        models
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

/// Explicit metadata adapter for a custom gateway or local runtime that has no
/// models.dev identity. Rich OpenRouter-style entries carry display name,
/// context length, pricing, and tool support; plain OpenAI-shaped gateways
/// return bare ids. Models that declare no tool support are dropped because
/// trouve's agent loop requires tools.
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
                .or_else(|| entry["context_window"].as_u64())
                .or_else(|| entry["max_model_len"].as_u64())
                .or_else(|| entry["max_context_length"].as_u64())
                .or_else(|| entry["input_token_limit"].as_u64())
                .or_else(|| entry["inputTokenLimit"].as_u64())
                .filter(|w| *w > 0)
                .unwrap_or(0);
            let reported_tools = entry["supported_parameters"]
                .as_array()
                .map(|p| p.iter().any(|v| v.as_str() == Some("tools")));
            let price = |k: &str| -> Option<f64> {
                entry["pricing"][k]
                    .as_str()
                    .and_then(|value| value.parse::<f64>().ok())
                    .or_else(|| entry["pricing"][k].as_f64())
                    .map(|value| value * 1e6)
            };
            // The protocol cannot represent unknown tool capability yet;
            // preserve generic OpenAI-compatible gateways until it can.
            let supports_tools = reported_tools.unwrap_or(true);
            supports_tools.then(|| trouve_protocol::ModelInfo {
                id: format!("{provider_id}/{name}"),
                display_name: entry["name"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| name.to_string()),
                context_window: window,
                supports_tools,
                input_price_per_mtok: price("prompt"),
                output_price_per_mtok: price("completion"),
                options_schema: serde_json::json!({}),
            })
        })
        .collect()
}

/// Intersect a catalog-covered provider's live ids with models.dev. All
/// metadata and option schemas come from the catalog regardless of richer
/// fields a vendor happens to include in its availability response.
fn parse_catalog_models(
    provider_id: &str,
    body: &Value,
    catalog_provider: &str,
    catalog: &ModelsDevCatalog,
) -> Vec<trouve_protocol::ModelInfo> {
    let Some(data) = body["data"].as_array() else {
        return Vec::new();
    };
    catalog.provider_models_for_ids(
        catalog_provider,
        provider_id,
        data.iter().filter_map(|entry| entry["id"].as_str()),
        OptionsDialect::OpenAi,
    )
}

fn apply_ollama_metadata(model: &mut trouve_protocol::ModelInfo, body: &Value) {
    if model.context_window == 0 {
        model.context_window = body["model_info"]
            .as_object()
            .into_iter()
            .flat_map(|metadata| metadata.iter())
            .filter(|(key, _)| key.ends_with(".context_length"))
            .filter_map(|(_, value)| value.as_u64())
            .filter(|value| *value > 0)
            .max()
            .unwrap_or(0);
    }
    // Newer Ollama versions report this explicitly. Absence means an older
    // server, not lack of tool support, so only override when the array exists.
    if let Some(capabilities) = body["capabilities"].as_array() {
        model.supports_tools = capabilities
            .iter()
            .any(|capability| capability.as_str() == Some("tools"));
    }
}

fn apply_lm_studio_metadata(
    models: &mut [trouve_protocol::ModelInfo],
    provider_id: &str,
    body: &Value,
) {
    let entries = body["models"]
        .as_array()
        .or_else(|| body["data"].as_array());
    let Some(entries) = entries else {
        return;
    };
    let prefix = format!("{provider_id}/");
    for model in models.iter_mut().filter(|model| model.context_window == 0) {
        let model_id = model.id.strip_prefix(&prefix).unwrap_or(&model.id);
        let Some(entry) = entries.iter().find(|entry| {
            entry["id"].as_str() == Some(model_id) || entry["key"].as_str() == Some(model_id)
        }) else {
            continue;
        };
        // A loaded instance's configured window is the effective serving
        // limit. Otherwise use the model's native maximum.
        model.context_window = entry["loaded_instances"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|instance| instance.pointer("/config/context_length")?.as_u64())
            .filter(|value| *value > 0)
            .min()
            .or_else(|| entry["max_context_length"].as_u64())
            .unwrap_or(0);
        if let Some(name) = entry["display_name"]
            .as_str()
            .filter(|name| !name.is_empty())
        {
            model.display_name = name.to_string();
        }
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
        // Known providers can omit `/models` entirely. Their models.dev roster
        // is the last-known catalog; arbitrary custom endpoints still return
        // no invented fallback.
        self.catalog_provider_id()
            .map(|provider| {
                self.catalog
                    .provider_models(&provider, &self.id, OptionsDialect::OpenAi)
            })
            .unwrap_or_default()
    }

    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        // Keep the lock through the refresh so concurrent callers share one
        // network request instead of stampeding an endpoint as the TTL turns.
        let mut cache = self.models_cache.lock().await;
        if let Some((at, models)) = cache.as_ref()
            && at.elapsed() < MODELS_TTL
        {
            return self.canonicalize_models(models.clone());
        }
        let stale = cache.as_ref().map(|(_, models)| models.clone());
        let fetched = tokio::time::timeout(MODELS_REQUEST_TIMEOUT, self.fetch_models())
            .await
            .map_err(|_| {
                ProviderError::Request(format!(
                    "model list timed out after {}s",
                    MODELS_REQUEST_TIMEOUT.as_secs_f64()
                ))
            })
            .and_then(|result| result);
        match fetched {
            Ok(models) => {
                *cache = Some((std::time::Instant::now(), models.clone()));
                self.canonicalize_models(models)
            }
            Err(e) => {
                tracing::debug!(
                    "{} model list failed: {e}; using stale/models.dev list",
                    self.id
                );
                stale
                    .map(|models| self.canonicalize_models(models))
                    .unwrap_or_else(|| self.models())
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
            .authed(
                self.client
                    .post(format!("{}/chat/completions", self.base_url)),
                &key,
            )
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
        let mut thinking_active = false;
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
                    if thinking_active {
                        let _ = tx
                            .send(Ok(ProviderEvent::ThinkingCompleted {
                                id: "reasoning".into(),
                            }))
                            .await;
                    }
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
                    let prompt_tokens = u["prompt_tokens"].as_u64().unwrap_or(0);
                    usage.output_tokens = u["completion_tokens"].as_u64().unwrap_or(0);
                    usage.cached_input_tokens = u
                        .pointer("/prompt_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    // OpenAI includes cached tokens in prompt_tokens, while
                    // Anthropic reports them separately. Normalize Usage so
                    // engine accounting and context math have one meaning.
                    usage.input_tokens = prompt_tokens.saturating_sub(usage.cached_input_tokens);
                    usage.cost_usd = u.get("cost").and_then(Value::as_f64);
                }
                let Some(delta) = v.pointer("/choices/0/delta") else {
                    continue;
                };
                if let Some(text) = delta.get("content").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    if thinking_active {
                        let _ = tx
                            .send(Ok(ProviderEvent::ThinkingCompleted {
                                id: "reasoning".into(),
                            }))
                            .await;
                        thinking_active = false;
                    }
                    let _ = tx
                        .send(Ok(ProviderEvent::TextDelta(text.to_string())))
                        .await;
                }
                // DeepSeek-style reasoning stream on chat completions.
                if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    if !thinking_active {
                        let _ = tx
                            .send(Ok(ProviderEvent::ThinkingStarted {
                                id: "reasoning".into(),
                            }))
                            .await;
                        thinking_active = true;
                    }
                    let _ = tx
                        .send(Ok(ProviderEvent::ThinkingDelta {
                            id: "reasoning".into(),
                            text: text.to_string(),
                        }))
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
        if thinking_active {
            let _ = tx
                .send(Ok(ProviderEvent::ThinkingCompleted {
                    id: "reasoning".into(),
                }))
                .await;
        }
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

    #[tokio::test]
    async fn reasoning_lifecycle_survives_interleaved_tool_call_deltas() {
        let payload = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"inspect\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",",
            "\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\" continue\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let source = futures::stream::iter([Ok::<_, reqwest::Error>(bytes::Bytes::from(payload))]);
        let events: Vec<_> = async_stream(source).collect().await;

        assert_eq!(events.len(), 6);
        assert!(matches!(
            &events[0],
            Ok(ProviderEvent::ThinkingStarted { id }) if id == "reasoning"
        ));
        assert!(matches!(
            &events[1],
            Ok(ProviderEvent::ThinkingDelta { id, text })
                if id == "reasoning" && text == "inspect"
        ));
        assert!(matches!(
            &events[2],
            Ok(ProviderEvent::ThinkingDelta { id, text })
                if id == "reasoning" && text == " continue"
        ));
        assert!(matches!(
            &events[3],
            Ok(ProviderEvent::ThinkingCompleted { id }) if id == "reasoning"
        ));
        assert!(matches!(
            &events[4],
            Ok(ProviderEvent::ToolCall(call))
                if call.id == "call-1" && call.name == "read_file"
        ));
        assert!(matches!(&events[5], Ok(ProviderEvent::Completed { .. })));
    }

    #[test]
    fn custom_header_and_query_auth_replace_bearer_auth() {
        let provider = OpenAiCompatProvider::new("custom", "https://example.test/v1", "unused")
            .with_http_options(
                false,
                BTreeMap::from([("x-api-key".into(), "secret".into())]),
                BTreeMap::from([("tenant".into(), "acme".into())]),
            );
        let request = provider
            .authed(provider.client.get("https://example.test/models"), "unused")
            .build()
            .unwrap();
        assert!(request.headers().get("authorization").is_none());
        assert_eq!(request.headers()["x-api-key"], "secret");
        assert_eq!(request.url().query(), Some("tenant=acme"));
    }

    #[test]
    fn catalog_provider_uses_live_ids_and_models_dev_metadata() {
        // The live record deliberately disagrees with models.dev. Only its id
        // is an availability signal.
        let body = json!({ "data": [
            {
                "id": "anthropic/claude-sonnet-4.5",
                "name": "Vendor override",
                "context_length": 8_192,
                "pricing": { "prompt": "0.000099", "completion": "0.000099" },
                "supported_parameters": ["max_tokens", "tools", "temperature"],
            },
            {
                "id": "some/no-tools-model",
                "name": "No Tools",
                "context_length": 8192,
                "supported_parameters": ["max_tokens"],
            },
        ]});
        let catalog = ModelsDevCatalog::embedded();
        let models = parse_catalog_models("kilocode", &body, "kilo", &catalog);
        assert_eq!(models.len(), 1, "unknown models are dropped");
        let m = &models[0];
        assert_eq!(m.id, "kilocode/anthropic/claude-sonnet-4.5");
        assert_eq!(m.display_name, "Anthropic: Claude Sonnet 4.5");
        assert_eq!(m.context_window, 1_000_000);
        assert_eq!(m.input_price_per_mtok, Some(3.0));
        assert_eq!(m.output_price_per_mtok, Some(15.0));
        assert_eq!(
            m.options_schema
                .pointer("/properties/reasoning_effort/default"),
            Some(&json!("medium"))
        );
    }

    #[test]
    fn custom_gateway_keeps_explicit_live_metadata_adapter() {
        let body = json!({ "data": [{
            "id": "custom-chat",
            "name": "Custom Chat",
            "context_length": 65_536,
            "pricing": { "prompt": "0.000002", "completion": "0.000004" },
            "supported_parameters": ["tools"],
        }]});
        let models = parse_gateway_models("custom", &body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].display_name, "Custom Chat");
        assert_eq!(models[0].context_window, 65_536);
        assert_eq!(models[0].input_price_per_mtok, Some(2.0));
    }

    #[test]
    fn parses_bare_openai_shaped_model_list() {
        // Plain gateways (ollama, vllm) return ids only. Preserve the model
        // but do not invent a context window.
        let body = json!({ "data": [ { "id": "qwen2.5-coder:7b" } ] });
        let models = parse_gateway_models("ollama", &body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "ollama/qwen2.5-coder:7b");
        assert_eq!(models[0].context_window, 0);
        assert!(models[0].supports_tools);
        assert_eq!(models[0].input_price_per_mtok, None);
    }

    #[test]
    fn parses_documented_compatible_context_fields() {
        let body = json!({ "data": [
            { "id": "groq-model", "context_window": 131_072 },
            { "id": "vllm-model", "max_model_len": 65_536 },
        ] });
        let models = parse_gateway_models("custom", &body);
        assert_eq!(models[0].context_window, 131_072);
        assert_eq!(models[1].context_window, 65_536);
    }

    #[test]
    fn applies_ollama_native_context_and_capabilities() {
        let mut model = trouve_protocol::ModelInfo {
            id: "ollama/qwen3:8b".into(),
            display_name: "qwen3:8b".into(),
            context_window: 0,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: json!({}),
        };
        apply_ollama_metadata(
            &mut model,
            &json!({
                "capabilities": ["completion", "tools"],
                "model_info": {
                    "general.architecture": "qwen3",
                    "qwen3.context_length": 262_144,
                }
            }),
        );
        assert_eq!(model.context_window, 262_144);
        assert!(model.supports_tools);
    }

    #[test]
    fn applies_lm_studio_effective_context() {
        let body = json!({ "data": [{ "id": "qwen", }] });
        let mut models = parse_gateway_models("lmstudio", &body);
        apply_lm_studio_metadata(
            &mut models,
            "lmstudio",
            &json!({"models": [{
                "key": "qwen",
                "display_name": "Qwen 3",
                "max_context_length": 262_144,
                "loaded_instances": [{"config": {"context_length": 65_536}}],
            }]}),
        );
        assert_eq!(models[0].context_window, 65_536);
        assert_eq!(models[0].display_name, "Qwen 3");
    }

    #[test]
    fn canonical_openai_list_is_live_with_models_dev_metadata() {
        let body = json!({ "data": [
            {"id": "gpt-5.6"},
            {"id": "text-embedding-4-large"},
            {"id": "gpt-5.6-terra"}
        ]});
        let catalog = ModelsDevCatalog::embedded();
        let models = parse_catalog_models("openai", &body, "openai", &catalog);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "openai/gpt-5.6");
        assert_eq!(models[0].context_window, 1_050_000);
        assert_eq!(models[1].input_price_per_mtok, Some(2.5));
    }

    #[test]
    fn known_endpoint_has_catalog_fallback_when_models_api_is_missing() {
        let provider =
            OpenAiCompatProvider::new("openai", "https://api.openai.com/v1", "unused-test-key");
        let models = provider.models();
        assert!(models.iter().any(|model| model.id == "openai/gpt-5.6"));
        assert!(models.iter().all(|model| !model.id.contains("embedding")));
    }

    #[tokio::test]
    async fn model_list_timeout_uses_stale_cache() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _connection = listener.accept().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(1));
        });

        let provider =
            OpenAiCompatProvider::new("custom", format!("http://{address}/v1"), "unused");
        let stale = trouve_protocol::ModelInfo {
            id: "custom/stale".into(),
            display_name: "Stale".into(),
            context_window: 8_192,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: json!({}),
        };
        *provider.models_cache.lock().await =
            Some((std::time::Instant::now() - MODELS_TTL, vec![stale.clone()]));

        let started = std::time::Instant::now();
        let models = provider.list_models().await;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, stale.id);
        assert_eq!(models[0].context_window, stale.context_window);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
