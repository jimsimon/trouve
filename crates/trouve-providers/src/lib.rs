//! LLM provider abstraction for the trouve harness.
//!
//! Implementations: OpenAI-compatible chat completions (also covers
//! OpenRouter, Ollama, vLLM, and most gateways via `base_url`) and the
//! Anthropic Messages API. Auth is pluggable ([`auth::TokenSource`]): static
//! API keys, or OAuth tokens with refresh (device flow / PKCE subscription
//! auth) persisted in the OS keychain ([`secrets`]).

pub mod anthropic;
pub mod auth;
pub mod catalog;
pub mod codex_responses;
pub mod openai_compat;
pub mod secrets;

use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use trouve_protocol::Usage;

/// A tool the model may call, in JSON Schema form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Conversation messages, provider-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        content: String,
        tool_calls: Vec<ToolCallRequest>,
    },
    ToolResult {
        call_id: String,
        content: String,
    },
}

/// Streamed output of one model invocation.
#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    /// Reasoning ("thinking") text, where the model/provider exposes it.
    ThinkingDelta(String),
    ToolCall(ToolCallRequest),
    Completed { usage: Usage },
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider request failed: {0}")]
    Request(String),
    #[error("provider returned an error: {0}")]
    Api(String),
    #[error("missing credentials: {0}")]
    Auth(String),
}

pub type EventStream = BoxStream<'static, Result<ProviderEvent, ProviderError>>;

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Stable identifier used as the prefix of model ids ("openai/gpt-4.1").
    fn id(&self) -> &str;

    /// Known models with capability/options schemas and pricing. Empty when
    /// the provider can't enumerate its models (custom gateways).
    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        Vec::new()
    }

    /// Run one model turn, streaming deltas and tool calls. `options` are
    /// model-specific knobs (temperature, reasoning effort, ...), already
    /// validated by the caller.
    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError>;
}
