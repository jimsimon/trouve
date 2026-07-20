//! Amazon Bedrock ConverseStream transport using the AWS SDK credential and
//! region chains. No long-lived AWS secret is copied into Trouve's config.

use std::collections::HashMap;
use std::sync::Arc;

use aws_sdk_bedrockruntime::types as aws;
use aws_smithy_types::{Document, Number};
use base64::Engine as _;
use serde_json::{Map, Value, json};
use trouve_protocol::Usage;

use crate::models_dev::{ModelsDevCatalog, OptionsDialect};
use crate::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
};

pub struct BedrockProvider {
    id: String,
    region: Option<String>,
    profile: Option<String>,
    catalog: Arc<ModelsDevCatalog>,
    client: tokio::sync::OnceCell<aws_sdk_bedrockruntime::Client>,
}

impl BedrockProvider {
    pub fn new(
        id: impl Into<String>,
        region: Option<String>,
        profile: Option<String>,
        catalog: Arc<ModelsDevCatalog>,
    ) -> Self {
        Self {
            id: id.into(),
            region,
            profile,
            catalog,
            client: tokio::sync::OnceCell::new(),
        }
    }

    async fn client(&self) -> Result<&aws_sdk_bedrockruntime::Client, ProviderError> {
        self.client
            .get_or_try_init(|| async {
                let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
                if let Some(region) = &self.region {
                    loader = loader.region(aws_config::Region::new(region.clone()));
                }
                if let Some(profile) = &self.profile {
                    loader = loader.profile_name(profile);
                }
                let config = loader.load().await;
                if config.region().is_none() {
                    return Err(ProviderError::Auth(
                        "Amazon Bedrock requires AWS_REGION, an AWS profile region, or a Region setting"
                            .into(),
                    ));
                }
                Ok(aws_sdk_bedrockruntime::Client::new(&config))
            })
            .await
    }

    fn wire_messages(
        messages: &[Message],
    ) -> Result<(Vec<aws::SystemContentBlock>, Vec<aws::Message>), ProviderError> {
        let mut system = Vec::new();
        let mut wire = Vec::new();
        for message in messages {
            let (role, blocks) = match message {
                Message::System(text) => {
                    system.push(aws::SystemContentBlock::Text(text.clone()));
                    continue;
                }
                Message::User(text) => (
                    aws::ConversationRole::User,
                    vec![aws::ContentBlock::Text(text.clone())],
                ),
                Message::Assistant {
                    content,
                    tool_calls,
                    reasoning,
                } => {
                    let mut blocks = Vec::new();
                    for block in reasoning {
                        if let Some(block) = replay_reasoning(block)? {
                            blocks.push(block);
                        }
                    }
                    if !content.is_empty() {
                        blocks.push(aws::ContentBlock::Text(content.clone()));
                    }
                    for call in tool_calls {
                        let tool = aws::ToolUseBlock::builder()
                            .tool_use_id(&call.id)
                            .name(&call.name)
                            .input(json_to_document(&call.arguments))
                            .build()
                            .map_err(build_error)?;
                        blocks.push(aws::ContentBlock::ToolUse(tool));
                    }
                    (aws::ConversationRole::Assistant, blocks)
                }
                Message::ToolResult {
                    call_id,
                    content,
                    images,
                } => {
                    let mut result = aws::ToolResultBlock::builder()
                        .tool_use_id(call_id)
                        .content(aws::ToolResultContentBlock::Text(content.clone()));
                    for image in images {
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(&image.data)
                            .map_err(|error| ProviderError::Request(error.to_string()))?;
                        let format = match image.mime.as_str() {
                            "image/gif" => aws::ImageFormat::Gif,
                            "image/jpeg" => aws::ImageFormat::Jpeg,
                            "image/png" => aws::ImageFormat::Png,
                            "image/webp" => aws::ImageFormat::Webp,
                            other => {
                                return Err(ProviderError::Request(format!(
                                    "Bedrock does not support tool image type {other}"
                                )));
                            }
                        };
                        let image = aws::ImageBlock::builder()
                            .format(format)
                            .source(aws::ImageSource::Bytes(bytes.into()))
                            .build()
                            .map_err(build_error)?;
                        result = result.content(aws::ToolResultContentBlock::Image(image));
                    }
                    let result = result.build().map_err(build_error)?;
                    (
                        aws::ConversationRole::User,
                        vec![aws::ContentBlock::ToolResult(result)],
                    )
                }
            };
            if blocks.is_empty() {
                continue;
            }
            wire.push(
                aws::Message::builder()
                    .role(role)
                    .set_content(Some(blocks))
                    .build()
                    .map_err(build_error)?,
            );
        }
        Ok((system, wire))
    }

    fn tool_config(tools: &[ToolSpec]) -> Result<Option<aws::ToolConfiguration>, ProviderError> {
        if tools.is_empty() {
            return Ok(None);
        }
        let mut config = aws::ToolConfiguration::builder();
        for tool in tools {
            let spec = aws::ToolSpecification::builder()
                .name(&tool.name)
                .description(&tool.description)
                .input_schema(aws::ToolInputSchema::Json(json_to_document(
                    &tool.parameters,
                )))
                .build()
                .map_err(build_error)?;
            config = config.tools(aws::Tool::ToolSpec(spec));
        }
        Ok(Some(config.build().map_err(build_error)?))
    }
}

#[async_trait::async_trait]
impl Provider for BedrockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.catalog
            .provider_models("amazon-bedrock", &self.id, OptionsDialect::Anthropic)
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Result<EventStream, ProviderError> {
        let client = self.client().await?;
        let (system, wire) = Self::wire_messages(messages)?;
        let mut request = client.converse_stream().model_id(model);
        for message in wire {
            request = request.messages(message);
        }
        for block in system {
            request = request.system(block);
        }
        if let Some(config) = Self::tool_config(tools)? {
            request = request.tool_config(config);
        }

        let mut inference = aws::InferenceConfiguration::builder();
        if let Some(value) = options.get("max_tokens").and_then(Value::as_i64) {
            inference = inference.max_tokens(value.clamp(1, i32::MAX as i64) as i32);
        }
        if let Some(value) = options.get("temperature").and_then(Value::as_f64) {
            inference = inference.temperature(value as f32);
        }
        if let Some(value) = options.get("top_p").and_then(Value::as_f64) {
            inference = inference.top_p(value as f32);
        }
        request = request.inference_config(inference.build());

        let additional = additional_model_fields(model, options);
        if !additional.is_empty() {
            request = request
                .additional_model_request_fields(json_to_document(&Value::Object(additional)));
        }
        let output = request
            .send()
            .await
            .map_err(|error| ProviderError::Api(error.to_string()))?;
        Ok(bedrock_events(output.stream))
    }
}

/// Translate Trouve's catalog-backed controls to the provider-specific
/// fields accepted through Bedrock Converse. Unrecognized controls remain
/// available to custom Bedrock models without inventing model capabilities.
fn additional_model_fields(model: &str, options: &Map<String, Value>) -> Map<String, Value> {
    let mut additional: Map<String, Value> = options
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "max_tokens"
                    | "temperature"
                    | "top_p"
                    | "effort"
                    | "thinking_level"
                    | "thinking_budget_tokens"
            )
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let effort = options.get("effort").and_then(Value::as_str);
    let thinking_level = options.get("thinking_level").and_then(Value::as_str);
    let budget = options
        .get("thinking_budget_tokens")
        .and_then(Value::as_u64);

    if model.contains("amazon.nova-2") {
        if thinking_level == Some("off") {
            additional.insert("reasoningConfig".into(), json!({"type": "disabled"}));
        } else if effort.is_some() || thinking_level == Some("on") {
            let mut config = json!({"type": "enabled"});
            if let Some(effort) = effort {
                config["maxReasoningEffort"] = json!(effort);
            }
            additional.insert("reasoningConfig".into(), config);
        }
    } else if model.contains("anthropic.claude") {
        // Adaptive effort takes precedence when a catalog record offers both
        // adaptive and legacy fixed-budget controls.
        if let Some(effort) = effort {
            additional.insert("thinking".into(), json!({"type": "adaptive"}));
            additional.insert("output_config".into(), json!({"effort": effort}));
        } else if let Some(budget) = budget {
            additional.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
        } else if thinking_level == Some("on") {
            additional.insert("thinking".into(), json!({"type": "adaptive"}));
        }
        // `off` is represented by omitting `thinking`; adaptive-only Claude
        // records do not advertise that toggle in models.dev.
    } else {
        for name in ["effort", "thinking_level", "thinking_budget_tokens"] {
            if let Some(value) = options.get(name) {
                additional.insert(name.into(), value.clone());
            }
        }
    }
    additional
}

#[derive(Default)]
struct PartialTool {
    id: String,
    name: String,
    input: String,
}

#[derive(Default)]
struct PartialReasoning {
    text: String,
    signature: String,
    redacted: Vec<u8>,
}

fn bedrock_events(
    mut receiver: aws_sdk_bedrockruntime::primitives::event_stream::EventReceiver<
        aws::ConverseStreamOutput,
        aws::error::ConverseStreamOutputError,
    >,
) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        let mut tools: HashMap<i32, PartialTool> = HashMap::new();
        let mut reasoning: HashMap<i32, PartialReasoning> = HashMap::new();
        let mut usage = Usage::default();
        loop {
            let event = match receiver.recv().await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    let _ = tx.send(Err(ProviderError::Api(error.to_string()))).await;
                    return;
                }
            };
            match event {
                aws::ConverseStreamOutput::ContentBlockStart(event) => {
                    if let Some(aws::ContentBlockStart::ToolUse(start)) = event.start() {
                        tools.insert(
                            event.content_block_index(),
                            PartialTool {
                                id: start.tool_use_id().into(),
                                name: start.name().into(),
                                input: String::new(),
                            },
                        );
                    }
                }
                aws::ConverseStreamOutput::ContentBlockDelta(event) => {
                    let index = event.content_block_index();
                    match event.delta() {
                        Some(aws::ContentBlockDelta::Text(text)) if !text.is_empty() => {
                            let _ = tx.send(Ok(ProviderEvent::TextDelta(text.clone()))).await;
                        }
                        Some(aws::ContentBlockDelta::ToolUse(delta)) => {
                            tools
                                .entry(index)
                                .or_default()
                                .input
                                .push_str(delta.input());
                        }
                        Some(aws::ContentBlockDelta::ReasoningContent(delta)) => {
                            let state = reasoning.entry(index).or_default();
                            match delta {
                                aws::ReasoningContentBlockDelta::Text(text) => {
                                    state.text.push_str(text);
                                    let _ = tx
                                        .send(Ok(ProviderEvent::ThinkingDelta(text.clone())))
                                        .await;
                                }
                                aws::ReasoningContentBlockDelta::Signature(signature) => {
                                    state.signature.push_str(signature);
                                }
                                aws::ReasoningContentBlockDelta::RedactedContent(data) => {
                                    state.redacted.extend_from_slice(data.as_ref());
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                aws::ConverseStreamOutput::ContentBlockStop(event) => {
                    let index = event.content_block_index();
                    if let Some(tool) = tools.remove(&index) {
                        let arguments = serde_json::from_str(&tool.input).unwrap_or(Value::Null);
                        let _ = tx
                            .send(Ok(ProviderEvent::ToolCall(ToolCallRequest {
                                id: tool.id,
                                name: tool.name,
                                arguments,
                            })))
                            .await;
                    }
                    if let Some(reasoning) = reasoning.remove(&index) {
                        let _ = tx
                            .send(Ok(ProviderEvent::Reasoning(reasoning_json(reasoning))))
                            .await;
                    }
                }
                aws::ConverseStreamOutput::Metadata(metadata) => {
                    if let Some(tokens) = metadata.usage() {
                        usage.input_tokens = tokens.input_tokens().max(0) as u64;
                        usage.output_tokens = tokens.output_tokens().max(0) as u64;
                        usage.cached_input_tokens =
                            tokens.cache_read_input_tokens().unwrap_or(0).max(0) as u64;
                    }
                }
                _ => {}
            }
        }
        for (_, tool) in tools {
            let arguments = serde_json::from_str(&tool.input).unwrap_or(Value::Null);
            let _ = tx
                .send(Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: tool.id,
                    name: tool.name,
                    arguments,
                })))
                .await;
        }
        let _ = tx.send(Ok(ProviderEvent::Completed { usage })).await;
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}

fn reasoning_json(reasoning: PartialReasoning) -> Value {
    json!({
        "transport": "amazon-bedrock",
        "text": reasoning.text,
        "signature": reasoning.signature,
        "redacted": base64::engine::general_purpose::STANDARD.encode(reasoning.redacted),
    })
}

fn replay_reasoning(value: &Value) -> Result<Option<aws::ContentBlock>, ProviderError> {
    if value["transport"].as_str() != Some("amazon-bedrock") {
        return Ok(None);
    }
    if let Some(encoded) = value["redacted"]
        .as_str()
        .filter(|encoded| !encoded.is_empty())
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| ProviderError::Request(error.to_string()))?;
        return Ok(Some(aws::ContentBlock::ReasoningContent(
            aws::ReasoningContentBlock::RedactedContent(bytes.into()),
        )));
    }
    let Some(text) = value["text"].as_str() else {
        return Ok(None);
    };
    let mut block = aws::ReasoningTextBlock::builder().text(text);
    if let Some(signature) = value["signature"]
        .as_str()
        .filter(|signature| !signature.is_empty())
    {
        block = block.signature(signature);
    }
    Ok(Some(aws::ContentBlock::ReasoningContent(
        aws::ReasoningContentBlock::ReasoningText(block.build().map_err(build_error)?),
    )))
}

fn json_to_document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(value) => Document::Bool(*value),
        Value::Number(value) => {
            if let Some(value) = value.as_u64() {
                Document::Number(Number::PosInt(value))
            } else if let Some(value) = value.as_i64() {
                Document::Number(Number::NegInt(value))
            } else {
                Document::Number(Number::Float(value.as_f64().unwrap_or_default()))
            }
        }
        Value::String(value) => Document::String(value.clone()),
        Value::Array(values) => Document::Array(values.iter().map(json_to_document).collect()),
        Value::Object(values) => Document::Object(
            values
                .iter()
                .map(|(name, value)| (name.clone(), json_to_document(value)))
                .collect(),
        ),
    }
}

fn build_error(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::Request(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_document_conversion_preserves_tool_schema() {
        let value = json!({"type": "object", "required": ["path"], "strict": true});
        let document = json_to_document(&value);
        assert!(document.as_object().is_some());
    }

    #[test]
    fn signed_reasoning_round_trips_through_provider_message() {
        let value = reasoning_json(PartialReasoning {
            text: "thinking".into(),
            signature: "signed".into(),
            redacted: Vec::new(),
        });
        assert!(replay_reasoning(&value).unwrap().is_some());
    }

    #[test]
    fn native_reasoning_controls_are_model_specific() {
        let claude = additional_model_fields(
            "us.anthropic.claude-fable-5",
            &Map::from_iter([("effort".into(), json!("high"))]),
        );
        assert_eq!(claude["thinking"], json!({"type": "adaptive"}));
        assert_eq!(claude["output_config"], json!({"effort": "high"}));

        let nova = additional_model_fields(
            "us.amazon.nova-2-lite-v1:0",
            &Map::from_iter([("effort".into(), json!("medium"))]),
        );
        assert_eq!(
            nova["reasoningConfig"],
            json!({"type": "enabled", "maxReasoningEffort": "medium"})
        );
    }

    #[test]
    fn offline_catalog_contains_bedrock_models() {
        let provider = BedrockProvider::new(
            "amazon-bedrock",
            Some("us-east-1".into()),
            None,
            Arc::new(ModelsDevCatalog::embedded()),
        );
        let models = provider.models();
        assert!(models.len() > 50, "only {} Bedrock models", models.len());
        let fable = models
            .iter()
            .find(|model| model.id.ends_with("/anthropic.claude-fable-5"))
            .unwrap();
        assert!(
            fable
                .options_schema
                .pointer("/properties/effort/enum")
                .is_some()
        );
        assert!(
            fable
                .options_schema
                .pointer("/properties/thinking_level")
                .is_none()
        );
    }
}
