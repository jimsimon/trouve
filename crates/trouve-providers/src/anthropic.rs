//! Anthropic Messages API provider (streaming).

use futures::StreamExt;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use trouve_protocol::Usage;

use crate::auth::TokenSource;
use crate::models_dev::{ModelsDevCatalog, OptionsDialect};
use crate::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
    catalog,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const FALLBACK_MAX_TOKENS: u64 = 8192;

pub struct AnthropicProvider {
    id: String,
    base_url: String,
    token: Arc<dyn TokenSource>,
    client: reqwest::Client,
    catalog: Arc<ModelsDevCatalog>,
    catalog_provider: Option<String>,
    /// Subscription (OAuth) tokens authenticate with `Authorization: Bearer`
    /// plus the oauth beta header instead of `x-api-key`.
    oauth_bearer: bool,
    /// GCP access token for Anthropic's Vertex `streamRawPredict` route.
    vertex_bearer: bool,
    default_auth: bool,
    headers: BTreeMap<String, String>,
    query_params: BTreeMap<String, String>,
    /// Live `/v1/models` result, cached for [`MODELS_TTL`].
    models_cache: tokio::sync::Mutex<Option<(std::time::Instant, Vec<trouve_protocol::ModelInfo>)>>,
    /// Model-specific maximum output from the same API. ModelInfo does not
    /// expose output limits, but Messages requires us to send max_tokens.
    output_limits: tokio::sync::Mutex<HashMap<String, u64>>,
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
            catalog: Arc::new(ModelsDevCatalog::embedded()),
            catalog_provider: None,
            oauth_bearer: false,
            vertex_bearer: false,
            default_auth: true,
            headers: BTreeMap::new(),
            query_params: BTreeMap::new(),
            models_cache: tokio::sync::Mutex::new(None),
            output_limits: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelsDevCatalog>) -> Self {
        self.catalog = catalog;
        self
    }

    pub fn with_catalog_provider(mut self, provider: impl Into<String>) -> Self {
        self.catalog_provider = Some(provider.into());
        self
    }

    pub fn with_http_options(
        mut self,
        default_auth: bool,
        headers: BTreeMap<String, String>,
        query_params: BTreeMap<String, String>,
    ) -> Self {
        self.default_auth = default_auth;
        self.headers = headers;
        self.query_params = query_params;
        self
    }

    fn catalog_provider_id(&self) -> Option<String> {
        self.catalog_provider.clone().or_else(|| {
            self.catalog
                .provider_for_endpoint(&self.id, &self.base_url, "anthropic")
        })
    }

    /// Add auth headers (API key or OAuth bearer) to a request.
    fn authed(&self, mut req: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
        req = req.header("anthropic-version", ANTHROPIC_VERSION);
        if self.vertex_bearer {
            req = req.bearer_auth(key);
        } else if self.default_auth && self.oauth_bearer {
            req = req
                .bearer_auth(key)
                .header("anthropic-beta", "oauth-2025-04-20");
        } else if self.default_auth {
            req = req.header("x-api-key", key);
        }
        for (name, value) in &self.headers {
            req = req.header(name.as_str(), value.as_str());
        }
        if !self.query_params.is_empty() {
            req = req.query(&self.query_params);
        }
        req
    }

    /// Fetch the account's model list from `/v1/models`, keeping the static
    /// catalog's pricing where ids match (the API doesn't report pricing).
    async fn fetch_models(&self) -> Result<Vec<trouve_protocol::ModelInfo>, ProviderError> {
        if self.vertex_bearer {
            return Err(ProviderError::Api(
                "Vertex AI does not expose Anthropic's Models API".into(),
            ));
        }
        let key = self.token.bearer().await?;
        let resp = self
            .authed(
                self.client
                    .get(format!("{}/v1/models?limit=1000", self.base_url)),
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
        let limits = parse_output_limits(&body);
        if !limits.is_empty() {
            self.output_limits.lock().await.extend(limits);
        }
        let catalog_provider = self.catalog_provider_id();
        Ok(parse_model_list(
            &self.id,
            &body,
            catalog_provider.as_deref(),
            &self.catalog,
        ))
    }

    /// Resolve the model's required Messages `max_tokens` cap. The normal
    /// model-list refresh populates this cache; direct/headless use lazily
    /// retrieves just the selected model.
    async fn output_limit(&self, model: &str, key: &str) -> Option<u64> {
        if let Some(limit) = self.output_limits.lock().await.get(model).copied() {
            return Some(limit);
        }
        let fetched = async {
            if self.vertex_bearer {
                return None;
            }
            let resp = self
                .authed(
                    self.client
                        .get(format!("{}/v1/models/{model}", self.base_url)),
                    key,
                )
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?;
            let body: Value = resp.json().await.ok()?;
            body["max_tokens"].as_u64().filter(|n| *n > 0)
        }
        .await;
        let limit = fetched.or_else(|| {
            self.catalog_provider_id()
                .and_then(|provider| self.catalog.output_limit(&provider, model))
        })?;
        self.output_limits
            .lock()
            .await
            .insert(model.to_string(), limit);
        Some(limit)
    }

    pub fn with_oauth_bearer(mut self) -> Self {
        self.oauth_bearer = true;
        self
    }

    /// Use Anthropic's Messages schema through Vertex AI's
    /// `streamRawPredict` route and GCP bearer authentication.
    pub fn with_vertex_bearer(mut self) -> Self {
        self.vertex_bearer = true;
        self.default_auth = false;
        self
    }

    fn request_url(&self, model: &str, body: &mut Value) -> Result<String, ProviderError> {
        if !self.vertex_bearer {
            return Ok(format!("{}/v1/messages", self.base_url));
        }
        body.as_object_mut().unwrap().remove("model");
        body["anthropic_version"] = json!("vertex-2023-10-16");
        let mut url = reqwest::Url::parse(&self.base_url)
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        url.path_segments_mut()
            .map_err(|_| ProviderError::Request("Vertex endpoint cannot be a base URL".into()))?
            .push("models")
            .push(&format!("{model}:streamRawPredict"));
        Ok(url.to_string())
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
                    reasoning,
                } => {
                    let mut blocks = Vec::new();
                    // Signed thinking blocks must come first and be replayed
                    // verbatim, or the API rejects a follow-up tool-use turn.
                    blocks.extend(reasoning.iter().cloned());
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
                Message::ToolResult {
                    call_id,
                    content,
                    images,
                } => {
                    // Tool results are user-role content blocks; consecutive
                    // results merge into the previous user message if it is
                    // already a block list. Images ride inside the result as
                    // native vision blocks.
                    let content_value = if images.is_empty() {
                        json!(content)
                    } else {
                        let mut blocks = vec![json!({"type": "text", "text": content})];
                        blocks.extend(images.iter().map(|img| {
                            json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": img.mime,
                                    "data": img.data,
                                }
                            })
                        }));
                        json!(blocks)
                    };
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": content_value,
                    });
                    if let Some(last) = wire.last_mut()
                        && last["role"] == "user"
                        && last["content"].is_array()
                    {
                        last["content"].as_array_mut().unwrap().push(block);
                        continue;
                    }
                    wire.push(json!({"role": "user", "content": [block]}));
                }
            }
        }
        (system, wire)
    }
}

/// Map the Models API's per-model capability records into the schemas clients
/// render. Older API versions omitted `capabilities`; only that case uses the
/// curated model fallback rather than inventing levels for a partial record.
fn parse_model_list(
    provider_id: &str,
    body: &Value,
    catalog_provider_id: Option<&str>,
    models_dev: &ModelsDevCatalog,
) -> Vec<trouve_protocol::ModelInfo> {
    let Some(data) = body["data"].as_array() else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|entry| {
            let name = entry["id"].as_str()?;
            let known = catalog_provider_id.and_then(|catalog_provider| {
                models_dev.model(
                    catalog_provider,
                    provider_id,
                    name,
                    OptionsDialect::Anthropic,
                )
            });
            let window = entry["max_input_tokens"]
                .as_u64()
                .filter(|w| *w > 0)
                .or_else(|| known.as_ref().map(|model| model.context_window))
                .unwrap_or(0);
            Some(trouve_protocol::ModelInfo {
                id: format!("{provider_id}/{name}"),
                display_name: entry["display_name"].as_str().unwrap_or(name).to_string(),
                context_window: window,
                supports_tools: true,
                input_price_per_mtok: known.as_ref().and_then(|model| model.input_price_per_mtok),
                output_price_per_mtok: known.as_ref().and_then(|model| model.output_price_per_mtok),
                options_schema: model_options_schema(
                    name,
                    entry.get("capabilities"),
                    catalog_provider_id,
                    models_dev,
                ),
            })
        })
        .collect()
}

fn parse_output_limits(body: &Value) -> HashMap<String, u64> {
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some((
                entry["id"].as_str()?.to_string(),
                entry["max_tokens"].as_u64().filter(|n| *n > 0)?,
            ))
        })
        .collect()
}

fn model_options_schema(
    model: &str,
    capabilities: Option<&Value>,
    catalog_provider_id: Option<&str>,
    models_dev: &ModelsDevCatalog,
) -> Value {
    let catalog_schema = || {
        catalog_provider_id
            .and_then(|provider| {
                models_dev.model(provider, provider, model, OptionsDialect::Anthropic)
            })
            .map(|model| model.options_schema)
            .unwrap_or_else(catalog::anthropic_plain_schema)
    };
    let Some(capabilities) = capabilities else {
        return catalog_schema();
    };

    let effort = &capabilities["effort"];
    if effort["supported"].as_bool() == Some(true) {
        let levels: Vec<&str> = effort
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(level, value)| {
                level.as_str() != "supported" && value["supported"].as_bool() == Some(true)
            })
            .map(|(level, _)| level.as_str())
            .collect();
        if !levels.is_empty() {
            return catalog::anthropic_effort_schema(&levels);
        }
    }

    match capabilities
        .pointer("/thinking/supported")
        .and_then(Value::as_bool)
    {
        Some(true) => catalog_schema(),
        Some(false) | None => catalog::anthropic_plain_schema(),
    }
}

fn apply_model_options(body: &mut Value, options: &Map<String, Value>) {
    for (key, value) in options {
        match key.as_str() {
            // Compatibility for threads saved before numeric budgets were
            // exposed directly.
            "thinking_level" => match value.as_str() {
                Some("on") => body["thinking"] = json!({"type": "adaptive"}),
                Some("off") => {
                    body.as_object_mut().unwrap().remove("thinking");
                    body.as_object_mut().unwrap().remove("output_config");
                }
                level => {
                    if let Some(budget) = level.and_then(catalog::thinking_budget_tokens) {
                        body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
                    }
                }
            },
            "thinking_budget_tokens" => {
                body["thinking"] = json!({"type": "enabled", "budget_tokens": value});
            }
            // Anthropic's effort control is nested and applies to adaptive
            // thinking. A top-level `effort` field is rejected by Messages.
            "effort" => {
                body["thinking"] = json!({"type": "adaptive"});
                body["output_config"] = json!({"effort": value});
            }
            _ => body[key.as_str()] = value.clone(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.catalog_provider_id()
            .map(|provider| {
                self.catalog
                    .provider_models(&provider, &self.id, OptionsDialect::Anthropic)
            })
            .unwrap_or_default()
    }

    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
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
                tracing::debug!("anthropic model list failed: {e}; using models.dev cache");
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
        let key = self.token.bearer().await?;
        let reported_output_limit = self.output_limit(model, &key).await;
        let max_tokens = reported_output_limit.unwrap_or(FALLBACK_MAX_TOKENS);
        let (system, wire) = Self::wire_messages(messages);
        let mut body = json!({
            "model": model,
            "max_tokens": max_tokens,
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
        apply_model_options(&mut body, options);
        // API constraints with thinking enabled: max_tokens must exceed the
        // budget, and temperature must stay at its default.
        if let Some(budget) = body
            .pointer("/thinking/budget_tokens")
            .and_then(Value::as_u64)
        {
            let max = body["max_tokens"].as_u64().unwrap_or(max_tokens);
            if max <= budget {
                if reported_output_limit.is_some() {
                    return Err(ProviderError::Request(format!(
                        "thinking budget {budget} must be smaller than model {model}'s {max}-token output limit"
                    )));
                }
                body["max_tokens"] = json!(budget + FALLBACK_MAX_TOKENS);
            }
        }
        if body.get("thinking").is_some() {
            body.as_object_mut().unwrap().remove("temperature");
        }

        let request_url = self.request_url(model, &mut body)?;

        let req = self.authed(self.client.post(request_url), &key);
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
    /// Thinking-block accumulation (for replay preservation).
    is_thinking: bool,
    is_redacted: bool,
    thinking_text: String,
    thinking_signature: String,
    redacted_data: String,
}

fn sse_to_events(
    mut bytes: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
    + Send
    + Unpin
    + 'static,
) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<ProviderEvent, ProviderError>>(64);
    tokio::spawn(async move {
        let mut buf = crate::sse::LineBuffer::default();
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
            buf.push(&chunk);
            while let Some(line) = buf.next_line() {
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                match v["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        let ordinary = v
                            .pointer("/message/usage/input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        let cache_write = v
                            .pointer("/message/usage/cache_creation_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        // Normalize to uncached input + cache reads. Cache
                        // creation still belongs in the active context and
                        // is conservatively priced as ordinary input.
                        usage.input_tokens = ordinary.saturating_add(cache_write);
                        usage.cached_input_tokens = v
                            .pointer("/message/usage/cache_read_input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    }
                    "content_block_start" => {
                        let idx = v["index"].as_u64().unwrap_or(0);
                        let block = blocks.entry(idx).or_default();
                        match v.pointer("/content_block/type").and_then(Value::as_str) {
                            Some("tool_use") => {
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
                            Some("thinking") => block.is_thinking = true,
                            Some("redacted_thinking") => {
                                block.is_redacted = true;
                                block.redacted_data = v
                                    .pointer("/content_block/data")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                            }
                            _ => {}
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
                                    blocks.entry(idx).or_default().thinking_text.push_str(text);
                                    let _ = tx
                                        .send(Ok(ProviderEvent::ThinkingDelta(text.to_string())))
                                        .await;
                                }
                            }
                            Some("signature_delta") => {
                                if let Some(sig) =
                                    v.pointer("/delta/signature").and_then(Value::as_str)
                                {
                                    blocks
                                        .entry(idx)
                                        .or_default()
                                        .thinking_signature
                                        .push_str(sig);
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
                            } else if block.is_thinking && !block.thinking_signature.is_empty() {
                                // Preserve the signed thinking block so it can
                                // be replayed on the next request.
                                let _ = tx
                                    .send(Ok(ProviderEvent::Reasoning(json!({
                                        "type": "thinking",
                                        "thinking": block.thinking_text,
                                        "signature": block.thinking_signature,
                                    }))))
                                    .await;
                            } else if block.is_redacted {
                                let _ = tx
                                    .send(Ok(ProviderEvent::Reasoning(json!({
                                        "type": "redacted_thinking",
                                        "data": block.redacted_data,
                                    }))))
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
    fn model_list_uses_reported_effort_levels() {
        let catalog = ModelsDevCatalog::embedded();
        let body = json!({
            "data": [{
                "id": "claude-fable-5",
                "display_name": "Claude Fable 5",
                "max_input_tokens": 1_000_000,
                "max_tokens": 128_000,
                "capabilities": {
                    "effort": {
                        "supported": true,
                        "low": {"supported": true},
                        "medium": {"supported": true},
                        "high": {"supported": true},
                        "xhigh": {"supported": false},
                        "max": {"supported": true}
                    },
                    "thinking": {
                        "supported": true,
                        "types": {"adaptive": {"supported": true}}
                    }
                }
            }]
        });
        let models = parse_model_list("anthropic", &body, Some("anthropic"), &catalog);
        let schema = &models[0].options_schema;
        assert_eq!(
            schema.pointer("/properties/effort/enum").unwrap(),
            &json!(["low", "medium", "high", "max"])
        );
        assert!(schema.pointer("/properties/thinking_level").is_none());
        assert_eq!(parse_output_limits(&body)["claude-fable-5"], 128_000);
    }

    #[test]
    fn model_list_does_not_invent_thinking_when_unsupported() {
        let catalog = ModelsDevCatalog::embedded();
        let body = json!({
            "data": [{
                "id": "claude-no-thinking",
                "capabilities": {
                    "effort": {"supported": false},
                    "thinking": {"supported": false}
                }
            }]
        });
        let models = parse_model_list("anthropic", &body, Some("anthropic"), &catalog);
        let properties = models[0].options_schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 1);
        assert!(properties.contains_key("temperature"));
    }

    #[test]
    fn old_model_records_use_models_dev_fallback() {
        let catalog = ModelsDevCatalog::embedded();
        let body = json!({"data": [{"id": "claude-fable-5"}]});
        let models = parse_model_list("anthropic", &body, Some("anthropic"), &catalog);
        assert_eq!(
            models[0]
                .options_schema
                .pointer("/properties/effort/enum")
                .unwrap(),
            &json!(["low", "medium", "high", "xhigh", "max"])
        );
    }

    #[test]
    fn effort_enables_adaptive_thinking_in_output_config() {
        let mut body = json!({"temperature": 0.4});
        let options = Map::from_iter([("effort".into(), json!("xhigh"))]);
        apply_model_options(&mut body, &options);
        assert_eq!(body["thinking"], json!({"type": "adaptive"}));
        assert_eq!(body["output_config"], json!({"effort": "xhigh"}));
    }

    #[test]
    fn vertex_route_moves_the_model_to_stream_raw_predict() {
        let provider = AnthropicProvider::new(
            "google-vertex-anthropic",
            Some(
                "https://us-east5-aiplatform.googleapis.com/v1/projects/test/locations/us-east5/publishers/anthropic"
                    .into(),
            ),
            Arc::new(crate::auth::StaticToken("test".into())),
        )
        .with_vertex_bearer();
        let mut body = json!({"model": "claude-sonnet-4-6", "stream": true});
        let url = provider
            .request_url("claude-sonnet-4-6@default", &mut body)
            .unwrap();
        assert!(url.ends_with("/models/claude-sonnet-4-6@default:streamRawPredict"));
        assert!(body.get("model").is_none());
        assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
    }

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
                reasoning: vec![],
            },
            Message::ToolResult {
                call_id: "t1".into(),
                content: "r1".into(),
                images: vec![],
            },
            Message::ToolResult {
                call_id: "t2".into(),
                content: "r2".into(),
                images: vec![],
            },
        ];
        let (system, wire) = AnthropicProvider::wire_messages(&messages);
        assert_eq!(system, "sys");
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[2]["role"], "user");
        assert_eq!(wire[2]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn reasoning_blocks_lead_the_assistant_turn() {
        let messages = [Message::Assistant {
            content: "answer".into(),
            tool_calls: vec![ToolCallRequest {
                id: "t1".into(),
                name: "a".into(),
                arguments: json!({}),
            }],
            reasoning: vec![json!({
                "type": "thinking",
                "thinking": "hmm",
                "signature": "sig123",
            })],
        }];
        let (_, wire) = AnthropicProvider::wire_messages(&messages);
        let blocks = wire[0]["content"].as_array().unwrap();
        // Signed thinking must be the first block, then text, then tool_use.
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["signature"], "sig123");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[2]["type"], "tool_use");
    }

    #[test]
    fn tool_result_images_become_vision_blocks() {
        let messages = [Message::ToolResult {
            call_id: "t1".into(),
            content: "image read".into(),
            images: vec![crate::ToolImage {
                mime: "image/png".into(),
                data: "QUJD".into(),
            }],
        }];
        let (_, wire) = AnthropicProvider::wire_messages(&messages);
        let blocks = wire[0]["content"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(blocks[1]["source"]["data"], "QUJD");
    }
}
