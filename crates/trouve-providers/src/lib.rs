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
pub mod codex;
pub mod codex_responses;
pub mod kimi_usage;
pub mod openai_compat;
pub mod secrets;
pub(crate) mod sse;

use futures::StreamExt;
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

/// An image a tool returned (vision content for multimodal models).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolImage {
    /// e.g. "image/png".
    pub mime: String,
    /// Base64-encoded image bytes.
    pub data: String,
}

/// Conversation messages, provider-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    System(String),
    User(String),
    Assistant {
        content: String,
        tool_calls: Vec<ToolCallRequest>,
        /// Provider-native reasoning blocks to replay verbatim on the next
        /// request (Anthropic's signed `thinking`/`redacted_thinking`
        /// blocks). Anthropic rejects a follow-up tool-use turn whose
        /// thinking blocks aren't preserved, so these must survive the
        /// round-trip. Opaque to other providers, which ignore them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reasoning: Vec<serde_json::Value>,
    },
    ToolResult {
        call_id: String,
        content: String,
        /// Images alongside the text (read_file on an image); providers
        /// render them as native vision input where supported.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ToolImage>,
    },
}

/// Streamed output of one model invocation.
#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    /// Reasoning ("thinking") text, where the model/provider exposes it.
    ThinkingDelta(String),
    /// A complete provider-native reasoning block to preserve for replay
    /// (Anthropic signed `thinking`/`redacted_thinking`). Distinct from
    /// `ThinkingDelta`, which is display-only streaming text.
    Reasoning(serde_json::Value),
    ToolCall(ToolCallRequest),
    Completed {
        usage: Usage,
    },
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

const PROVIDER_DELTA_WINDOW: std::time::Duration = std::time::Duration::from_millis(16);
const PROVIDER_DELTA_MAX_BYTES: usize = 64 * 1024;

struct CoalescingProviderStream {
    stream: EventStream,
    pending: Option<Result<ProviderEvent, ProviderError>>,
}

/// Normalize transport-selected text chunking for every native provider.
/// Adjacent text/thinking fragments are losslessly concatenated for a short
/// window; control events keep their order and are never combined.
pub fn coalesce_event_stream(stream: EventStream) -> EventStream {
    Box::pin(futures::stream::unfold(
        CoalescingProviderStream {
            stream,
            pending: None,
        },
        |mut state| async move {
            let mut event = match state.pending.take() {
                Some(event) => event,
                None => state.stream.next().await?,
            };
            let mut fragments = 1_u64;
            let started = std::time::Instant::now();
            if provider_event_delta_len(&event).is_some_and(|len| len < PROVIDER_DELTA_MAX_BYTES) {
                let deadline = tokio::time::Instant::now() + PROVIDER_DELTA_WINDOW;
                loop {
                    let next = match tokio::time::timeout_at(deadline, state.stream.next()).await {
                        Ok(Some(next)) => next,
                        Ok(None) | Err(_) => break,
                    };
                    match merge_provider_event(&mut event, next) {
                        Ok(()) => fragments += 1,
                        Err(next) => {
                            state.pending = Some(next);
                            break;
                        }
                    }
                }
            }
            if fragments > 1 {
                tracing::trace!(
                    input_fragments = fragments,
                    output_events = 1,
                    output_bytes = provider_event_delta_len(&event).unwrap_or_default(),
                    elapsed_us = started.elapsed().as_micros(),
                    "native provider deltas coalesced"
                );
            }
            Some((event, state))
        },
    ))
}

fn provider_event_delta_len(event: &Result<ProviderEvent, ProviderError>) -> Option<usize> {
    match event {
        Ok(ProviderEvent::TextDelta(text) | ProviderEvent::ThinkingDelta(text)) => Some(text.len()),
        _ => None,
    }
}

fn merge_provider_event(
    existing: &mut Result<ProviderEvent, ProviderError>,
    incoming: Result<ProviderEvent, ProviderError>,
) -> Result<(), Result<ProviderEvent, ProviderError>> {
    match (&mut *existing, incoming) {
        (Ok(ProviderEvent::TextDelta(current)), Ok(ProviderEvent::TextDelta(next)))
            if current.len().saturating_add(next.len()) <= PROVIDER_DELTA_MAX_BYTES =>
        {
            current.push_str(&next);
            Ok(())
        }
        (Ok(ProviderEvent::ThinkingDelta(current)), Ok(ProviderEvent::ThinkingDelta(next)))
            if current.len().saturating_add(next.len()) <= PROVIDER_DELTA_MAX_BYTES =>
        {
            current.push_str(&next);
            Ok(())
        }
        (_, incoming) => Err(incoming),
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Stable identifier used as the prefix of model ids ("openai/gpt-4.1").
    fn id(&self) -> &str;

    /// Live model listing, for providers whose catalog can be queried.
    /// Defaults to the static snapshot.
    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.models()
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_provider_deltas_are_coalesced_without_crossing_boundaries() {
        let source: EventStream = Box::pin(futures::stream::iter(vec![
            Ok(ProviderEvent::TextDelta("a".into())),
            Ok(ProviderEvent::TextDelta("b".into())),
            Ok(ProviderEvent::ThinkingDelta("c".into())),
            Ok(ProviderEvent::ThinkingDelta("d".into())),
            Ok(ProviderEvent::Completed {
                usage: Usage::default(),
            }),
        ]));
        let events: Vec<_> = coalesce_event_stream(source).collect().await;
        assert!(matches!(&events[0], Ok(ProviderEvent::TextDelta(text)) if text == "ab"));
        assert!(matches!(&events[1], Ok(ProviderEvent::ThinkingDelta(text)) if text == "cd"));
        assert!(matches!(&events[2], Ok(ProviderEvent::Completed { .. })));
    }
}
