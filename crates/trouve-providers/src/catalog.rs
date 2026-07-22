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
/// Subscription access goes through the vendors' own binaries (`auth:
/// "cli"` presets below) — never by hijacking their OAuth client
/// registrations. The generic OAuth machinery remains available for
/// providers that sanction third-party clients.
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
            category: provider_category(id, auth, base_url),
            experimental: false,
        }
    }

    let mut providers = models_dev.provider_presets();
    providers.extend([
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
        // Local runtimes and vendor CLI agent backends are Trouve
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
            "Cursor (Subscription)",
            "cursor-cli",
            None,
            None,
            "cli",
        ),
        p(
            "cursor-api",
            "Cursor (API Key)",
            "cursor-cli",
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
    ]);
    providers
}

/// Classify a configured provider for settings presentation. Authentication
/// and transport are deliberately independent from billing/presentation.
pub fn provider_category(id: &str, auth: &str, base_url: Option<&str>) -> String {
    if id == "kimi-code" || auth == "cli" || auth == "oauth" {
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
    fn provider_categories_are_independent_from_auth_and_wire_kind() {
        assert_eq!(
            provider_category("claude-code", "cli", None),
            "subscription"
        );
        assert_eq!(
            provider_category("kimi-code", "api-key", None),
            "subscription"
        );
        assert_eq!(provider_category("cursor-api", "api-key", None), "api");
        assert_eq!(
            provider_category("ollama", "none", Some("http://localhost:11434/v1")),
            "local"
        );
        assert_eq!(
            provider_category("custom", "api-key", Some("http://127.0.0.1:8000/v1")),
            "local"
        );
        for url in [
            "http://user:password@localhost:11434/v1",
            "http://localhost:11434/v1?model=local",
            "http://localhost:11434/v1#models",
        ] {
            assert_eq!(
                provider_category("custom", "api-key", Some(url)),
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
