//! Well-known provider setup presets and provider-level compatibility helpers.
//!
//! Model metadata lives in the refreshable models.dev catalog, not here.

use serde_json::json;
use trouve_protocol::KnownProvider;

/// Provider presets from the refreshable models.dev roster, plus integrations
/// that are specific to Trouve and therefore absent from that API. Clients
/// render these as one-click setup options; ids are suggestions, not
/// constraints.
///
/// Subscription access goes through vendor-supported credentials: their own
/// binaries where required, or product/SDK API keys for Kimi Code and Cursor.
/// It never hijacks vendor OAuth client registrations. The generic OAuth
/// machinery remains available for providers that sanction third-party
/// clients.
pub fn known_providers(models_dev: &crate::models_dev::ModelsDevCatalog) -> Vec<KnownProvider> {
    fn p(
        id: &str,
        display_name: &str,
        kind: &str,
        base_url: Option<&str>,
        api_key_env: Option<&str>,
        auth: &str,
    ) -> KnownProvider {
        KnownProvider {
            id: id.into(),
            display_name: display_name.into(),
            kind: kind.into(),
            base_url: base_url.map(Into::into),
            api_key_env: api_key_env.map(Into::into),
            config_fields: Vec::new(),
            headers: Default::default(),
            query_params: Default::default(),
            auth: auth.into(),
            category: provider_category(kind, auth, base_url),
            experimental: false,
        }
    }

    let providers = models_dev.provider_presets();
    let trouve_integrations = [
        // Kimi Code is billed as a subscription even though it authenticates
        // with an API-key-shaped token.
        p(
            "kimi-code",
            "Kimi Code (Subscription)",
            "openai-compat",
            Some(crate::kimi_usage::KIMI_CODE_BASE_URL),
            Some("KIMI_CODE_API_KEY"),
            "api-key",
        ),
        // Local runtimes and vendor agent backends are Trouve
        // integrations, not model API providers, so models.dev does not list
        // them.
        p(
            "ollama",
            "Ollama (local)",
            "openai-compat",
            Some("http://localhost:11434/v1"),
            None,
            "none",
        ),
        p(
            "codex",
            "Codex (ChatGPT Subscription)",
            "codex-app-server",
            None,
            None,
            "cli",
        ),
        p(
            "cursor",
            "Cursor (Agent SDK)",
            "cursor-sdk",
            None,
            Some("CURSOR_API_KEY"),
            "api-key",
        ),
        p(
            "claude-code",
            "Claude Code (Subscription)",
            "claude-cli",
            None,
            None,
            "cli",
        ),
    ];
    merge_provider_presets(providers, trouve_integrations)
}

fn merge_provider_presets(
    mut providers: Vec<KnownProvider>,
    trouve_integrations: impl IntoIterator<Item = KnownProvider>,
) -> Vec<KnownProvider> {
    let trouve_integrations: Vec<_> = trouve_integrations.into_iter().collect();
    let explicit_ids: std::collections::HashSet<_> = trouve_integrations
        .iter()
        .map(|provider| provider.id.as_str())
        .collect();
    providers.retain(|provider| !explicit_ids.contains(provider.id.as_str()));
    providers.extend(trouve_integrations);
    providers
}

/// Classify a configured provider for settings presentation. Trusted
/// integration kinds and canonical service endpoints may identify a
/// subscription; user-chosen provider ids never do.
pub fn provider_category(kind: &str, auth: &str, base_url: Option<&str>) -> String {
    if matches!(kind, "cursor-sdk" | "cursor-cli")
        || auth == "cli"
        || auth == "oauth"
        || crate::kimi_usage::is_kimi_code_base_url(base_url)
    {
        "subscription".into()
    } else if base_url.is_some_and(is_loopback_url) {
        "local".into()
    } else {
        "api".into()
    }
}

fn is_loopback_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

/// Schema for the exact effort values returned by Anthropic's live Models
/// API. The values are data from the response; no model family is inferred.
pub fn anthropic_effort_schema(levels: &[&str]) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "effort": {
                "type": "string",
                "enum": levels,
                "description": "Reasoning effort"
            },
            "temperature": {"type": "number", "minimum": 0.0, "maximum": 1.0}
        }
    })
}

/// API options when a live Anthropic record explicitly reports no reasoning
/// control, or when neither the API nor models.dev has one.
pub fn anthropic_plain_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "temperature": {"type": "number", "minimum": 0.0, "maximum": 1.0}
        }
    })
}

/// Compatibility translation for threads saved before fixed-budget models
/// switched from invented low/medium/high labels to the numeric
/// `thinking_budget_tokens` field supplied by models.dev. New schemas never
/// advertise these labels.
pub fn thinking_budget_tokens(level: &str) -> Option<u64> {
    match level {
        "low" => Some(4_096),
        "medium" => Some(16_384),
        "high" => Some(32_768),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_providers_merge_with_trouve_integrations() {
        let catalog = crate::models_dev::ModelsDevCatalog::embedded();
        let providers = known_providers(&catalog);
        let ids: std::collections::HashSet<_> = providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect();
        assert_eq!(ids.len(), providers.len());
        assert!(ids.contains("openrouter"));
        assert!(ids.contains("ollama"));
        assert!(ids.contains("codex"));
        assert!(ids.contains("claude-code"));
        assert!(!ids.contains("codex-api"));
    }

    #[test]
    fn trouve_integrations_replace_catalog_entries_with_the_same_id() {
        let catalog_entry = KnownProvider {
            id: "collision".into(),
            display_name: "Catalog".into(),
            kind: "openai-compat".into(),
            base_url: Some("https://catalog.example".into()),
            api_key_env: None,
            config_fields: Vec::new(),
            headers: Default::default(),
            query_params: Default::default(),
            auth: "api-key".into(),
            category: "api".into(),
            experimental: false,
        };
        let explicit = KnownProvider {
            display_name: "Trouve".into(),
            base_url: Some("https://trouve.example".into()),
            ..catalog_entry.clone()
        };
        let merged = merge_provider_presets(vec![catalog_entry], [explicit]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].display_name, "Trouve");
    }

    #[test]
    fn provider_categories_use_transport_and_endpoint_not_configurable_ids() {
        assert_eq!(provider_category("claude-cli", "cli", None), "subscription");
        assert_eq!(
            provider_category(
                "openai-compat",
                "api-key",
                Some(crate::kimi_usage::KIMI_CODE_BASE_URL)
            ),
            "subscription"
        );
        assert_eq!(
            provider_category("cursor-sdk", "api-key", None),
            "subscription"
        );
        assert_eq!(
            provider_category("openai-compat", "api-key", None),
            "api",
            "a generic API transport must not inherit a subscription category"
        );
        assert_eq!(
            provider_category("openai-compat", "none", Some("http://localhost:11434/v1")),
            "local"
        );
        assert_eq!(
            provider_category("openai-compat", "api-key", Some("http://127.0.0.1:8000/v1")),
            "local"
        );
        for url in [
            "http://user:password@localhost:11434/v1",
            "http://localhost:11434/v1?model=local",
            "http://localhost:11434/v1#models",
        ] {
            assert_eq!(
                provider_category("openai-compat", "api-key", Some(url)),
                "api",
                "non-canonical loopback URL should not be local: {url}"
            );
        }
    }

    #[test]
    fn effort_schema_preserves_only_reported_values() {
        let schema = anthropic_effort_schema(&["low", "high", "max"]);
        assert_eq!(
            schema.pointer("/properties/effort/enum"),
            Some(&json!(["low", "high", "max"]))
        );
        assert!(schema.pointer("/properties/effort/default").is_none());
    }

    #[test]
    fn legacy_budget_translation_is_not_an_advertised_schema() {
        assert_eq!(thinking_budget_tokens("low"), Some(4_096));
        assert_eq!(thinking_budget_tokens("off"), None);
    }
}
