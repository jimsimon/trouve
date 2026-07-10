//! Static model catalogs with per-model options schemas (clients render
//! model knobs from these schemas — no hardcoded per-model UI).
//!
//! Prices are list prices in USD per million tokens, used for cost
//! accounting; they are data, not truth — override in config when they
//! drift.

use serde_json::json;
use trouve_protocol::{KnownProvider, ModelInfo};

/// Well-known provider presets (the roster libraries like rig.rs cover),
/// mapped onto the two wire protocols we speak. Clients render these as
/// one-click setup options; ids are suggestions, not constraints.
///
/// Subscription access goes through the vendors' own binaries (`auth:
/// "cli"` presets below) — never by hijacking their OAuth client
/// registrations, which vendors treat as abuse and close accounts for
/// (Anthropic actively does). The generic OAuth machinery is supported for
/// providers that sanction third-party clients — configure
/// `[providers.<id>.oauth]` manually to opt in.
pub fn known_providers() -> Vec<KnownProvider> {
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
            auth: auth.into(),
            experimental: false,
        }
    }
    vec![
        p(
            "openai",
            "OpenAI (API key)",
            "openai-compat",
            Some("https://api.openai.com/v1"),
            Some("OPENAI_API_KEY"),
            "api-key",
        ),
        p(
            "anthropic",
            "Anthropic (API key)",
            "anthropic",
            Some("https://api.anthropic.com"),
            Some("ANTHROPIC_API_KEY"),
            "api-key",
        ),
        p(
            "gemini",
            "Google Gemini",
            "openai-compat",
            Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            Some("GEMINI_API_KEY"),
            "api-key",
        ),
        p(
            "xai",
            "xAI (Grok)",
            "openai-compat",
            Some("https://api.x.ai/v1"),
            Some("XAI_API_KEY"),
            "api-key",
        ),
        p(
            "deepseek",
            "DeepSeek",
            "openai-compat",
            Some("https://api.deepseek.com/v1"),
            Some("DEEPSEEK_API_KEY"),
            "api-key",
        ),
        p(
            "groq",
            "Groq",
            "openai-compat",
            Some("https://api.groq.com/openai/v1"),
            Some("GROQ_API_KEY"),
            "api-key",
        ),
        p(
            "mistral",
            "Mistral",
            "openai-compat",
            Some("https://api.mistral.ai/v1"),
            Some("MISTRAL_API_KEY"),
            "api-key",
        ),
        p(
            "openrouter",
            "OpenRouter",
            "openai-compat",
            Some("https://openrouter.ai/api/v1"),
            Some("OPENROUTER_API_KEY"),
            "api-key",
        ),
        p(
            "kilocode",
            "Kilo Code",
            "openai-compat",
            Some("https://api.kilo.ai/api/gateway"),
            Some("KILO_API_KEY"),
            "api-key",
        ),
        p(
            "perplexity",
            "Perplexity",
            "openai-compat",
            Some("https://api.perplexity.ai"),
            Some("PERPLEXITY_API_KEY"),
            "api-key",
        ),
        p(
            "together",
            "Together AI",
            "openai-compat",
            Some("https://api.together.xyz/v1"),
            Some("TOGETHER_API_KEY"),
            "api-key",
        ),
        p(
            "cohere",
            "Cohere",
            "openai-compat",
            Some("https://api.cohere.ai/compatibility/v1"),
            Some("COHERE_API_KEY"),
            "api-key",
        ),
        p(
            "moonshot",
            "Moonshot (Kimi)",
            "openai-compat",
            Some("https://api.moonshot.ai/v1"),
            Some("MOONSHOT_API_KEY"),
            "api-key",
        ),
        p(
            "ollama",
            "Ollama (local)",
            "openai-compat",
            Some("http://localhost:11434/v1"),
            None,
            "none",
        ),
        // Subscription agent backends: the vendor CLI runs the agent loop
        // and holds the auth; trouve orchestrates it (sanctioned surface).
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
        // Same cursor-agent binary, but authenticated with an API key
        // (usage-based billing) instead of the subscription login.
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
        // Direct client for ChatGPT's backend Codex endpoint using the token
        // from `codex login`. Tolerated, not contracted: undocumented, may
        // break or be restricted at any time.
        KnownProvider {
            id: "codex-api".into(),
            display_name: "Codex Direct (Experimental)".into(),
            kind: "codex-responses".into(),
            base_url: None,
            api_key_env: None,
            auth: "cli".into(),
            experimental: true,
        },
    ]
}

/// Static catalog for the experimental direct-Codex provider.
pub fn codex_models(provider_id: &str) -> Vec<ModelInfo> {
    let reasoning = json!({
        "type": "object",
        "properties": {
            "reasoning_effort": {
                "type": "string",
                "enum": ["low", "medium", "high"],
                "description": "How much thinking the model does before answering"
            }
        }
    });
    // Subscription-billed: no per-token pricing.
    vec![
        ModelInfo {
            id: format!("{provider_id}/gpt-5.4-codex"),
            display_name: "GPT-5.4 Codex (Experimental)".into(),
            context_window: 272_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: reasoning.clone(),
        },
        ModelInfo {
            id: format!("{provider_id}/gpt-5.4"),
            display_name: "GPT-5.4 (Experimental)".into(),
            context_window: 272_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: reasoning,
        },
    ]
}

pub fn openai_models(provider_id: &str) -> Vec<ModelInfo> {
    let reasoning = json!({
        "type": "object",
        "properties": {
            "reasoning_effort": {
                "type": "string",
                "enum": ["minimal", "low", "medium", "high"],
                "description": "How much thinking the model does before answering"
            }
        }
    });
    let plain = json!({
        "type": "object",
        "properties": {
            "temperature": {"type": "number", "minimum": 0.0, "maximum": 2.0}
        }
    });
    vec![
        ModelInfo {
            id: format!("{provider_id}/gpt-4.1"),
            display_name: "GPT-4.1".into(),
            context_window: 1_047_576,
            supports_tools: true,
            input_price_per_mtok: Some(2.00),
            output_price_per_mtok: Some(8.00),
            options_schema: plain.clone(),
        },
        ModelInfo {
            id: format!("{provider_id}/gpt-4.1-mini"),
            display_name: "GPT-4.1 mini".into(),
            context_window: 1_047_576,
            supports_tools: true,
            input_price_per_mtok: Some(0.40),
            output_price_per_mtok: Some(1.60),
            options_schema: plain,
        },
        ModelInfo {
            id: format!("{provider_id}/o4-mini"),
            display_name: "o4-mini".into(),
            context_window: 200_000,
            supports_tools: true,
            input_price_per_mtok: Some(1.10),
            output_price_per_mtok: Some(4.40),
            options_schema: reasoning,
        },
    ]
}

/// The options schema shared by all Claude models that support extended
/// thinking. Levels map to thinking budgets via [`thinking_budget_tokens`].
pub fn anthropic_thinking_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "thinking_level": {
                "type": "string",
                "enum": ["off", "low", "medium", "high"],
                "default": "off",
                "description": "Extended thinking budget"
            },
            "temperature": {"type": "number", "minimum": 0.0, "maximum": 1.0}
        }
    })
}

/// Token budget for a thinking level ("off"/unknown = thinking disabled).
/// Used by the Anthropic API provider (`thinking.budget_tokens`) and the
/// Claude Code backend (`MAX_THINKING_TOKENS`).
pub fn thinking_budget_tokens(level: &str) -> Option<u64> {
    match level {
        "low" => Some(4_096),
        "medium" => Some(16_384),
        "high" => Some(32_768),
        _ => None,
    }
}

/// Claude model catalog, shared by the per-use Anthropic API provider and
/// the Claude Code subscription backend so both surface the same list. The
/// API provider upgrades this with a live `/v1/models` fetch when it can.
///
/// List prices are flat across the full context window: Anthropic removed
/// the long-context (>200k input) surcharge in March 2026.
pub fn anthropic_models(provider_id: &str) -> Vec<ModelInfo> {
    let m = |name: &str, display: &str, window: u64, prices: Option<(f64, f64)>| ModelInfo {
        id: format!("{provider_id}/{name}"),
        display_name: display.into(),
        context_window: window,
        supports_tools: true,
        input_price_per_mtok: prices.map(|p| p.0),
        output_price_per_mtok: prices.map(|p| p.1),
        options_schema: anthropic_thinking_schema(),
    };
    vec![
        m(
            "claude-fable-5",
            "Claude Fable 5",
            1_000_000,
            Some((10.00, 50.00)),
        ),
        m(
            "claude-opus-4-8",
            "Claude Opus 4.8",
            1_000_000,
            Some((5.00, 25.00)),
        ),
        // Introductory pricing through Aug 31, 2026 ($3/$15 after).
        m(
            "claude-sonnet-5",
            "Claude Sonnet 5",
            1_000_000,
            Some((2.00, 10.00)),
        ),
        m(
            "claude-sonnet-4-6",
            "Claude Sonnet 4.6",
            1_000_000,
            Some((3.00, 15.00)),
        ),
        m(
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            200_000,
            Some((3.00, 15.00)),
        ),
        m(
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            200_000,
            Some((1.00, 5.00)),
        ),
    ]
}

/// Cost of a turn in USD given the model's pricing, when known.
pub fn cost_usd(model: &ModelInfo, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    let input = model.input_price_per_mtok?;
    let output = model.output_price_per_mtok?;
    Some((input_tokens as f64 * input + output_tokens as f64 * output) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_math() {
        let m = &openai_models("openai")[0]; // gpt-4.1: $2 in, $8 out
        let cost = cost_usd(m, 1_000_000, 500_000).unwrap();
        assert!((cost - 6.0).abs() < 1e-9);
    }
}
