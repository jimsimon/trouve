//! Native OpenAI Responses API provider.
//!
//! This adapter is intentionally separate from [`crate::openai_compat`]. The
//! official OpenAI API uses typed response items and encrypted reasoning
//! replay; generic OpenAI-compatible gateways continue to use Chat
//! Completions until they explicitly implement the Responses contract.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use futures::StreamExt;
use serde_json::{Map, Value, json};
use trouve_protocol::Usage;

use crate::auth::{StaticToken, TokenSource};
use crate::models_dev::ModelsDevCatalog;
use crate::openai_compat::OpenAiCompatProvider;
use crate::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
};

/// OpenAI's typed `/responses` transport. Model discovery still uses the
/// shared `/models` implementation because that endpoint is identical to the
/// one used by Chat Completions.
pub struct OpenAiResponsesProvider {
    id: String,
    base_url: String,
    token: Arc<dyn TokenSource>,
    client: reqwest::Client,
    model_source: OpenAiCompatProvider,
    bearer_auth: bool,
    headers: BTreeMap<String, String>,
    query_params: BTreeMap<String, String>,
}

impl OpenAiResponsesProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self::with_token(id, base_url, Arc::new(StaticToken(api_key.into())))
    }

    pub fn with_token(
        id: impl Into<String>,
        base_url: impl Into<String>,
        token: Arc<dyn TokenSource>,
    ) -> Self {
        let id = id.into();
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let model_source =
            OpenAiCompatProvider::with_token(id.clone(), base_url.clone(), token.clone());
        Self {
            id,
            base_url,
            token,
            client: reqwest::Client::new(),
            model_source,
            bearer_auth: true,
            headers: BTreeMap::new(),
            query_params: BTreeMap::new(),
        }
    }

    pub fn with_catalog(mut self, catalog: Arc<ModelsDevCatalog>) -> Self {
        self.model_source = self.model_source.with_catalog(catalog);
        self
    }

    pub fn with_catalog_provider(mut self, provider: impl Into<String>) -> Self {
        self.model_source = self.model_source.with_catalog_provider(provider);
        self
    }

    pub fn with_http_options(
        mut self,
        bearer_auth: bool,
        headers: BTreeMap<String, String>,
        query_params: BTreeMap<String, String>,
    ) -> Self {
        self.model_source =
            self.model_source
                .with_http_options(bearer_auth, headers.clone(), query_params.clone());
        self.bearer_auth = bearer_auth;
        self.headers = headers;
        self.query_params = query_params;
        self
    }

    /// Standard OpenAI endpoint with the key from `OPENAI_API_KEY`.
    pub fn openai_from_env() -> Result<Self, ProviderError> {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| ProviderError::Auth("OPENAI_API_KEY is not set".into()))?;
        Ok(Self::new("openai", "https://api.openai.com/v1", key).with_catalog_provider("openai"))
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

    fn wire_input(messages: &[Message]) -> Vec<Value> {
        let mut input = Vec::with_capacity(messages.len());
        for message in messages {
            match message {
                Message::System(text) => input.push(json!({
                    "role": "system",
                    "content": text,
                })),
                Message::User(text) => input.push(json!({
                    "role": "user",
                    "content": text,
                })),
                Message::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                } => {
                    // Stateless Responses requests must replay the exact
                    // encrypted reasoning item. Filter provider-native items
                    // so a mid-thread provider switch never sends Anthropic
                    // thinking blocks to OpenAI.
                    input.extend(
                        reasoning
                            .iter()
                            .filter(|item| item["type"].as_str() == Some("reasoning"))
                            .cloned(),
                    );
                    if !content.is_empty() {
                        input.push(json!({
                            "role": "assistant",
                            "content": content,
                        }));
                    }
                    input.extend(tool_calls.iter().map(|call| {
                        json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        })
                    }));
                }
                Message::ToolResult {
                    call_id,
                    content,
                    images,
                } => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content,
                    }));
                    // Keep function output itself portable and attach vision
                    // content as the immediately following user input.
                    if !images.is_empty() {
                        let mut parts = vec![json!({
                            "type": "input_text",
                            "text": format!("Image content from tool call {call_id}:"),
                        })];
                        parts.extend(images.iter().map(|image| {
                            json!({
                                "type": "input_image",
                                "image_url": format!("data:{};base64,{}", image.mime, image.data),
                            })
                        }));
                        input.push(json!({ "role": "user", "content": parts }));
                    }
                }
            }
        }
        input
    }

    fn request_body(
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Value {
        let mut body = json!({
            "model": model,
            "input": Self::wire_input(messages),
            "stream": true,
            "store": false,
            "parallel_tool_calls": true,
            "include": ["reasoning.encrypted_content"],
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(
                tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        })
                    })
                    .collect(),
            );
        }
        apply_options(&mut body, options);
        body
    }
}

fn nested_object<'a>(body: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !body[key].is_object() {
        body[key] = Value::Object(Map::new());
    }
    body[key]
        .as_object_mut()
        .expect("nested response option is an object")
}

/// Translate trouve's established OpenAI option names onto the Responses
/// request shape. Invariants that define this adapter (stateless streaming,
/// typed input, and tool ownership) cannot be overridden by model options.
fn apply_options(body: &mut Value, options: &Map<String, Value>) {
    for (key, value) in options {
        match key.as_str() {
            "reasoning_effort" | "thinking_level" => {
                let reasoning = nested_object(body, "reasoning");
                reasoning.insert("effort".into(), value.clone());
                reasoning
                    .entry("summary")
                    .or_insert_with(|| Value::String("auto".into()));
            }
            "reasoning_summary" => {
                nested_object(body, "reasoning").insert("summary".into(), value.clone());
            }
            "verbosity" => {
                nested_object(body, "text").insert("verbosity".into(), value.clone());
            }
            "max_tokens" | "max_completion_tokens" => {
                body["max_output_tokens"] = value.clone();
            }
            "model"
            | "input"
            | "tools"
            | "stream"
            | "store"
            | "include"
            | "parallel_tool_calls" => {}
            _ => body[key] = value.clone(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiResponsesProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.model_source.models()
    }

    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.model_source.list_models().await
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Result<EventStream, ProviderError> {
        let body = Self::request_body(model, messages, tools, options);
        let key = self.token.bearer().await?;
        let response = self
            .authed(
                self.client.post(format!("{}/responses", self.base_url)),
                &key,
            )
            .json(&body)
            .send()
            .await
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{status}: {text}")));
        }
        Ok(response_stream(response.bytes_stream()))
    }
}

fn response_usage(response: &Value) -> Usage {
    let usage = &response["usage"];
    let inclusive_input = usage["input_tokens"].as_u64().unwrap_or(0);
    let cached_input_tokens = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Usage {
        input_tokens: inclusive_input.saturating_sub(cached_input_tokens),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cached_input_tokens,
        context_input_tokens: Some(inclusive_input),
        cost_usd: None,
        context_window: None,
    }
}

fn output_item_key(item: &Value, output_index: usize) -> String {
    item["id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}:{output_index}", item["type"].as_str().unwrap_or("item")))
}

async fn emit_output_item(
    tx: &tokio::sync::mpsc::Sender<Result<ProviderEvent, ProviderError>>,
    item: &Value,
    output_index: usize,
    emitted_items: &mut HashSet<String>,
    streamed_text_items: &HashSet<String>,
    streamed_reasoning_items: &HashSet<String>,
) -> bool {
    let key = output_item_key(item, output_index);
    if !emitted_items.insert(key.clone()) {
        return true;
    }
    match item["type"].as_str() {
        Some("function_call") => {
            let Some(call_id) = item["call_id"].as_str().or_else(|| item["id"].as_str()) else {
                return tx
                    .send(Err(ProviderError::Api(
                        "Responses function call is missing call_id".into(),
                    )))
                    .await
                    .is_ok();
            };
            let Some(name) = item["name"].as_str() else {
                return tx
                    .send(Err(ProviderError::Api(
                        "Responses function call is missing name".into(),
                    )))
                    .await
                    .is_ok();
            };
            let arguments = match item["arguments"].as_str() {
                Some(arguments) => match serde_json::from_str(arguments) {
                    Ok(arguments) => arguments,
                    Err(error) => {
                        return tx
                            .send(Err(ProviderError::Api(format!(
                                "invalid Responses function arguments: {error}"
                            ))))
                            .await
                            .is_ok();
                    }
                },
                None => Value::Null,
            };
            tx.send(Ok(ProviderEvent::ToolCall(ToolCallRequest {
                id: call_id.to_string(),
                name: name.to_string(),
                arguments,
            })))
            .await
            .is_ok()
        }
        Some("reasoning") => {
            if !streamed_reasoning_items.contains(&key)
                && let Some(summary) = item["summary"].as_array()
            {
                for text in summary
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .filter(|text| !text.is_empty())
                {
                    if tx
                        .send(Ok(ProviderEvent::ThinkingDelta(text.to_string())))
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
            }
            tx.send(Ok(ProviderEvent::Reasoning(item.clone())))
                .await
                .is_ok()
        }
        Some("message") if !streamed_text_items.contains(&key) => {
            for text in item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|part| part["type"].as_str() == Some("output_text"))
                .filter_map(|part| part["text"].as_str())
                .filter(|text| !text.is_empty())
            {
                if tx
                    .send(Ok(ProviderEvent::TextDelta(text.to_string())))
                    .await
                    .is_err()
                {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

/// Decode typed Responses SSE events into trouve's provider-neutral stream.
fn response_stream(
    mut bytes: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
    + Send
    + Unpin
    + 'static,
) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut lines = crate::sse::LineBuffer::default();
        let mut emitted_items = HashSet::new();
        let mut streamed_text_items = HashSet::new();
        let mut streamed_reasoning_items = HashSet::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = tx
                        .send(Err(ProviderError::Request(error.to_string())))
                        .await;
                    return;
                }
            };
            lines.push(&chunk);
            while let Some(line) = lines.next_line() {
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let event: Value = match serde_json::from_str(data) {
                    Ok(event) => event,
                    Err(error) => {
                        let _ = tx
                            .send(Err(ProviderError::Api(format!(
                                "invalid Responses stream event: {error}"
                            ))))
                            .await;
                        return;
                    }
                };
                match event["type"].as_str() {
                    Some("response.output_text.delta") => {
                        if let Some(item_id) = event["item_id"].as_str() {
                            streamed_text_items.insert(item_id.to_string());
                        }
                        if let Some(delta) = event["delta"].as_str().filter(|text| !text.is_empty())
                            && tx
                                .send(Ok(ProviderEvent::TextDelta(delta.to_string())))
                                .await
                                .is_err()
                        {
                            return;
                        }
                    }
                    Some("response.reasoning_summary_text.delta") => {
                        if let Some(item_id) = event["item_id"].as_str() {
                            streamed_reasoning_items.insert(item_id.to_string());
                        }
                        if let Some(delta) = event["delta"].as_str().filter(|text| !text.is_empty())
                            && tx
                                .send(Ok(ProviderEvent::ThinkingDelta(delta.to_string())))
                                .await
                                .is_err()
                        {
                            return;
                        }
                    }
                    Some("response.output_item.done") => {
                        let output_index = event["output_index"].as_u64().unwrap_or(0) as usize;
                        if !emit_output_item(
                            &tx,
                            &event["item"],
                            output_index,
                            &mut emitted_items,
                            &streamed_text_items,
                            &streamed_reasoning_items,
                        )
                        .await
                        {
                            return;
                        }
                    }
                    Some("response.completed") => {
                        if let Some(output) = event["response"]["output"].as_array() {
                            for (index, item) in output.iter().enumerate() {
                                if !emit_output_item(
                                    &tx,
                                    item,
                                    index,
                                    &mut emitted_items,
                                    &streamed_text_items,
                                    &streamed_reasoning_items,
                                )
                                .await
                                {
                                    return;
                                }
                            }
                        }
                        let _ = tx
                            .send(Ok(ProviderEvent::Completed {
                                usage: response_usage(&event["response"]),
                            }))
                            .await;
                        return;
                    }
                    Some("response.failed") | Some("response.incomplete") => {
                        let message = event["response"]["error"]["message"]
                            .as_str()
                            .or_else(|| event["response"]["incomplete_details"]["reason"].as_str())
                            .unwrap_or("OpenAI response did not complete");
                        let _ = tx.send(Err(ProviderError::Api(message.to_string()))).await;
                        return;
                    }
                    Some("error") => {
                        let message = event["message"]
                            .as_str()
                            .or_else(|| event["error"]["message"].as_str())
                            .unwrap_or("OpenAI Responses stream error");
                        let _ = tx.send(Err(ProviderError::Api(message.to_string()))).await;
                        return;
                    }
                    _ => {}
                }
            }
        }
        let _ = tx
            .send(Err(ProviderError::Request(
                "Responses stream ended before response.completed".into(),
            )))
            .await;
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolImage;

    #[test]
    fn request_uses_typed_items_and_responses_options() {
        let messages = vec![
            Message::System("system".into()),
            Message::Assistant {
                content: String::new(),
                reasoning: vec![
                    json!({"type":"thinking", "signature":"anthropic"}),
                    json!({"type":"reasoning", "id":"rs_1", "encrypted_content":"opaque", "summary":[]}),
                ],
                tool_calls: vec![ToolCallRequest {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"README.md"}),
                }],
            },
            Message::ToolResult {
                call_id: "call_1".into(),
                content: "ok".into(),
                images: vec![ToolImage {
                    mime: "image/png".into(),
                    data: "AAAA".into(),
                }],
            },
        ];
        let options = Map::from_iter([
            ("reasoning_effort".into(), json!("high")),
            ("verbosity".into(), json!("low")),
            ("max_tokens".into(), json!(2048)),
        ]);
        let body = OpenAiResponsesProvider::request_body(
            "gpt-test",
            &messages,
            &[ToolSpec {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({"type":"object"}),
            }],
            &options,
        );

        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["text"]["verbosity"], "low");
        assert_eq!(body["max_output_tokens"], 2048);
        assert_eq!(body["tools"][0]["name"], "read_file");
        let input = body["input"].as_array().unwrap();
        assert!(input.iter().any(|item| item["type"] == "reasoning"));
        assert!(!input.iter().any(|item| item["type"] == "thinking"));
        assert!(input.iter().any(|item| item["type"] == "function_call"));
        assert!(
            input
                .iter()
                .any(|item| item["type"] == "function_call_output")
        );
        assert!(input.iter().any(|item| {
            item["role"] == "user"
                && item["content"]
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(|part| part["type"] == "input_image"))
        }));
    }

    #[tokio::test]
    async fn stream_preserves_reasoning_tools_text_and_usage() {
        let payload = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"opaque\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"thought\"}]}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"hello\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":2,\"item\":{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"opaque\",\"summary\":[]},{\"id\":\"msg_1\",\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]},{\"id\":\"fc_1\",\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}],\"usage\":{\"input_tokens\":100,\"input_tokens_details\":{\"cached_tokens\":25},\"output_tokens\":8}}}\n\n",
        );
        let source =
            futures::stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(payload))]);
        let events: Vec<_> = response_stream(source).collect().await;

        assert!(matches!(&events[0], Ok(ProviderEvent::ThinkingDelta(text)) if text == "thought"));
        assert!(matches!(&events[1], Ok(ProviderEvent::Reasoning(item)) if item["id"] == "rs_1"));
        assert!(matches!(&events[2], Ok(ProviderEvent::TextDelta(text)) if text == "hello"));
        assert!(
            matches!(&events[3], Ok(ProviderEvent::ToolCall(call)) if call.id == "call_1" && call.arguments["path"] == "README.md")
        );
        assert!(matches!(&events[4], Ok(ProviderEvent::Completed { usage })
            if usage.input_tokens == 75
                && usage.cached_input_tokens == 25
                && usage.output_tokens == 8
                && usage.context_input_tokens == Some(100)));
        assert_eq!(
            events.len(),
            5,
            "completed output items must not duplicate deltas"
        );
    }

    #[tokio::test]
    async fn truncated_stream_is_an_error_not_a_completed_turn() {
        let source =
            futures::stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from_static(
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
            ))]);
        let events: Vec<_> = response_stream(source).collect().await;
        assert!(matches!(&events[0], Ok(ProviderEvent::TextDelta(text)) if text == "partial"));
        assert!(
            matches!(&events[1], Err(ProviderError::Request(message)) if message.contains("before response.completed"))
        );
    }
}
