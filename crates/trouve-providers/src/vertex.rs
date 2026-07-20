//! Google Vertex AI native `streamGenerateContent` transport using
//! Application Default Credentials.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::StreamExt;
use serde_json::{Map, Value, json};
use trouve_protocol::Usage;

use crate::auth::TokenSource;
use crate::models_dev::{ModelsDevCatalog, OptionsDialect};
use crate::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
};

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Application Default Credentials token source shared by Google's native
/// Gemini and Anthropic-on-Vertex transports.
pub struct GoogleAccessToken {
    credentials_path: Option<String>,
    auth: tokio::sync::OnceCell<Arc<dyn gcp_auth::TokenProvider>>,
}

impl GoogleAccessToken {
    pub fn new(credentials_path: Option<String>) -> Self {
        Self {
            credentials_path,
            auth: tokio::sync::OnceCell::new(),
        }
    }

    async fn access_token(&self) -> Result<Arc<gcp_auth::Token>, ProviderError> {
        let provider = self
            .auth
            .get_or_try_init(|| async {
                if let Some(path) = &self.credentials_path {
                    let provider = gcp_auth::CustomServiceAccount::from_file(path)
                        .map_err(|error| ProviderError::Auth(error.to_string()))?;
                    return Ok(Arc::new(provider) as Arc<dyn gcp_auth::TokenProvider>);
                }
                gcp_auth::provider()
                    .await
                    .map_err(|error| ProviderError::Auth(error.to_string()))
            })
            .await?;
        provider
            .token(&[CLOUD_PLATFORM_SCOPE])
            .await
            .map_err(|error| ProviderError::Auth(error.to_string()))
    }
}

#[async_trait::async_trait]
impl TokenSource for GoogleAccessToken {
    async fn bearer(&self) -> Result<String, ProviderError> {
        Ok(self.access_token().await?.as_str().to_string())
    }
}

pub struct VertexProvider {
    id: String,
    base_url: String,
    catalog: Arc<ModelsDevCatalog>,
    client: reqwest::Client,
    token: GoogleAccessToken,
}

impl VertexProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        credentials_path: Option<String>,
        catalog: Arc<ModelsDevCatalog>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into().trim_end_matches('/').into(),
            catalog,
            client: reqwest::Client::new(),
            token: GoogleAccessToken::new(credentials_path),
        }
    }

    fn wire_messages(messages: &[Message]) -> (Option<Value>, Vec<Value>) {
        let mut system_parts = Vec::new();
        let mut wire = Vec::new();
        let mut tool_names: HashMap<String, String> = HashMap::new();
        for message in messages {
            match message {
                Message::System(text) => system_parts.push(json!({"text": text})),
                Message::User(text) => wire.push(json!({
                    "role": "user",
                    "parts": [{"text": text}],
                })),
                Message::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                } => {
                    let mut parts = Vec::new();
                    let mut replayed_calls = HashSet::new();
                    for block in reasoning {
                        if block["transport"].as_str() == Some("google-vertex")
                            && let Some(part) = block.get("part")
                        {
                            if let Some(call) = part.get("functionCall") {
                                if let Some(id) = call["id"].as_str() {
                                    replayed_calls.insert(id.to_string());
                                }
                                if let Some(name) = call["name"].as_str() {
                                    replayed_calls.insert(name.to_string());
                                }
                            }
                            parts.push(part.clone());
                        }
                    }
                    if !content.is_empty() {
                        parts.push(json!({"text": content}));
                    }
                    for call in tool_calls {
                        tool_names.insert(call.id.clone(), call.name.clone());
                        if replayed_calls.contains(&call.id) || replayed_calls.contains(&call.name)
                        {
                            continue;
                        }
                        parts.push(json!({
                            "functionCall": {
                                "id": call.id,
                                "name": call.name,
                                "args": call.arguments,
                            }
                        }));
                    }
                    if !parts.is_empty() {
                        wire.push(json!({"role": "model", "parts": parts}));
                    }
                }
                Message::ToolResult {
                    call_id,
                    content,
                    images,
                } => {
                    let name = tool_names
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| call_id.clone());
                    let mut parts = vec![json!({
                        "functionResponse": {
                            "id": call_id,
                            "name": name,
                            "response": {"output": content},
                        }
                    })];
                    parts.extend(images.iter().map(|image| {
                        json!({
                            "inlineData": {
                                "mimeType": image.mime,
                                "data": image.data,
                            }
                        })
                    }));
                    wire.push(json!({"role": "user", "parts": parts}));
                }
            }
        }
        let system = (!system_parts.is_empty()).then(|| json!({"parts": system_parts}));
        (system, wire)
    }

    fn request_body(
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Value {
        let (system, contents) = Self::wire_messages(messages);
        let mut body = json!({"contents": contents});
        if let Some(system) = system {
            body["systemInstruction"] = system;
        }
        if !tools.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": tools.iter().map(|tool| json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })).collect::<Vec<_>>()
            }]);
        }
        let mut generation = Map::new();
        for (name, value) in options {
            match name.as_str() {
                "top_p" => {
                    generation.insert("topP".into(), value.clone());
                }
                "max_tokens" | "max_output_tokens" => {
                    generation.insert("maxOutputTokens".into(), value.clone());
                }
                "thinking_level" | "reasoning_effort" => {
                    let config = generation
                        .entry("thinkingConfig")
                        .or_insert_with(|| json!({}));
                    match value.as_str() {
                        Some("off") if model.starts_with("gemini-2.5-") => {
                            config["thinkingBudget"] = json!(0);
                        }
                        Some("on") if model.starts_with("gemini-2.5-") => {
                            config["thinkingBudget"] = json!(-1);
                        }
                        Some(level) => {
                            config["thinkingLevel"] = json!(level.to_ascii_uppercase());
                        }
                        None => config["thinkingLevel"] = value.clone(),
                    }
                }
                "thinking_budget_tokens" => {
                    generation
                        .entry("thinkingConfig")
                        .or_insert_with(|| json!({}))["thinkingBudget"] = value.clone();
                }
                other => {
                    generation.insert(other.into(), value.clone());
                }
            }
        }
        if !generation.is_empty() {
            body["generationConfig"] = Value::Object(generation);
        }
        body
    }

    fn stream_url(&self, model: &str) -> Result<reqwest::Url, ProviderError> {
        let mut url = reqwest::Url::parse(&self.base_url)
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        url.path_segments_mut()
            .map_err(|_| ProviderError::Request("Vertex endpoint cannot be a base URL".into()))?
            .push("models")
            .push(&format!("{model}:streamGenerateContent"));
        url.query_pairs_mut().append_pair("alt", "sse");
        Ok(url)
    }
}

#[async_trait::async_trait]
impl Provider for VertexProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        let prefix = format!("{}/", self.id);
        self.catalog
            .provider_models("google-vertex", &self.id, OptionsDialect::Gemini)
            .into_iter()
            // This adapter targets publisher `google`. Partner MaaS models
            // have distinct Vertex transports and must not be sent to the
            // Gemini generateContent endpoint.
            .filter(|model| {
                model
                    .id
                    .strip_prefix(&prefix)
                    .is_some_and(|id| id.starts_with("gemini-"))
            })
            .collect()
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Result<EventStream, ProviderError> {
        let token = self.token.bearer().await?;
        let response = self
            .client
            .post(self.stream_url(model)?)
            .bearer_auth(token)
            .json(&Self::request_body(model, messages, tools, options))
            .send()
            .await
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!("{status}: {text}")));
        }
        Ok(vertex_events(response.bytes_stream()))
    }
}

fn vertex_events(
    mut bytes: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>>
    + Send
    + Unpin
    + 'static,
) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut buffer = crate::sse::LineBuffer::default();
        let mut usage = Usage::default();
        let mut call_index = 0_u64;
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
            buffer.push(&chunk);
            while let Some(line) = buffer.next_line() {
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    let _ = tx.send(Ok(ProviderEvent::Completed { usage })).await;
                    return;
                }
                let Ok(value) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(metadata) = value.get("usageMetadata") {
                    usage.input_tokens = metadata["promptTokenCount"].as_u64().unwrap_or(0);
                    usage.output_tokens = metadata["candidatesTokenCount"].as_u64().unwrap_or(0);
                    usage.cached_input_tokens =
                        metadata["cachedContentTokenCount"].as_u64().unwrap_or(0);
                }
                let Some(parts) = value
                    .pointer("/candidates/0/content/parts")
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                for part in parts {
                    if part.get("thoughtSignature").is_some() {
                        let _ = tx
                            .send(Ok(ProviderEvent::Reasoning(json!({
                                "transport": "google-vertex",
                                "part": part,
                            }))))
                            .await;
                    }
                    if let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) {
                        let event = if part["thought"].as_bool() == Some(true) {
                            ProviderEvent::ThinkingDelta(text.into())
                        } else {
                            ProviderEvent::TextDelta(text.into())
                        };
                        let _ = tx.send(Ok(event)).await;
                    }
                    if let Some(call) = part.get("functionCall") {
                        let name = call["name"].as_str().unwrap_or("vertex-tool");
                        let id = call["id"].as_str().map(String::from).unwrap_or_else(|| {
                            call_index += 1;
                            format!("vertex-{call_index}")
                        });
                        let _ = tx
                            .send(Ok(ProviderEvent::ToolCall(ToolCallRequest {
                                id,
                                name: name.into(),
                                arguments: call.get("args").cloned().unwrap_or(Value::Null),
                            })))
                            .await;
                    }
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
    fn request_maps_tools_and_native_thinking_config() {
        let body = VertexProvider::request_body(
            "gemini-3.1-pro-preview",
            &[Message::User("hello".into())],
            &[ToolSpec {
                name: "read".into(),
                description: "read a file".into(),
                parameters: json!({"type": "object"}),
            }],
            &Map::from_iter([("thinking_level".into(), json!("high"))]),
        );
        assert_eq!(
            body.pointer("/generationConfig/thinkingConfig/thinkingLevel"),
            Some(&json!("HIGH"))
        );

        let toggle = VertexProvider::request_body(
            "gemini-2.5-flash",
            &[Message::User("hello".into())],
            &[],
            &Map::from_iter([("thinking_level".into(), json!("off"))]),
        );
        assert_eq!(
            toggle.pointer("/generationConfig/thinkingConfig/thinkingBudget"),
            Some(&json!(0))
        );
        assert_eq!(
            body.pointer("/tools/0/functionDeclarations/0/name"),
            Some(&json!("read"))
        );
    }

    #[test]
    fn vertex_adapter_exposes_only_google_publisher_models() {
        let provider = VertexProvider::new(
            "google-vertex",
            "https://us-central1-aiplatform.googleapis.com/v1/projects/test/locations/us-central1/publishers/google",
            None,
            Arc::new(ModelsDevCatalog::embedded()),
        );
        let models = provider.models();
        assert!(!models.is_empty());
        assert!(
            models
                .iter()
                .all(|model| model.id.starts_with("google-vertex/gemini-"))
        );
    }
}
