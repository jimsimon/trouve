//! Azure OpenAI v1 transport.
//!
//! Azure's current data plane uses the OpenAI Chat Completions wire shape,
//! but API-key credentials live in the `api-key` header and endpoints are
//! resource-scoped. Keep that policy in an explicit adapter instead of
//! teaching every OpenAI-compatible provider about Azure.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::auth::TokenSource;
use crate::models_dev::ModelsDevCatalog;
use crate::openai_compat::OpenAiCompatProvider;
use crate::{EventStream, Message, Provider, ProviderError, ToolSpec};

pub struct AzureOpenAiProvider {
    inner: OpenAiCompatProvider,
    id: String,
}

impl AzureOpenAiProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        token: Arc<dyn TokenSource>,
        catalog: Arc<ModelsDevCatalog>,
        catalog_provider: impl Into<String>,
        headers: BTreeMap<String, String>,
        query_params: BTreeMap<String, String>,
    ) -> Self {
        let id = id.into();
        Self {
            inner: OpenAiCompatProvider::with_token(id.clone(), base_url, token)
                .with_catalog(catalog)
                .with_catalog_provider(catalog_provider)
                .with_http_options(false, headers, query_params),
            id,
        }
    }
}

#[async_trait::async_trait]
impl Provider for AzureOpenAiProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        let prefix = format!("{}/", self.id);
        self.inner
            .models()
            .into_iter()
            // Azure-hosted Claude deployments use the Anthropic Messages
            // transport, not this OpenAI v1 transport. Live `/models`
            // results remain authoritative and are not filtered because
            // deployment names are user-defined.
            .filter(|model| {
                !model
                    .id
                    .strip_prefix(&prefix)
                    .is_some_and(|id| id.starts_with("claude-"))
            })
            .collect()
    }

    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.inner.list_models().await
    }

    async fn stream_chat(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        options: &Map<String, Value>,
    ) -> Result<EventStream, ProviderError> {
        self.inner
            .stream_chat(model, messages, tools, options)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::StaticToken;

    #[test]
    fn azure_openai_fallback_does_not_advertise_messages_only_claude_models() {
        let provider = AzureOpenAiProvider::new(
            "azure",
            "https://example.openai.azure.com/openai/v1",
            Arc::new(StaticToken("test".into())),
            Arc::new(ModelsDevCatalog::embedded()),
            "azure",
            Default::default(),
            Default::default(),
        );
        let models = provider.models();
        assert!(!models.is_empty());
        assert!(!models.iter().any(|model| model.id.contains("/claude-")));
    }
}
