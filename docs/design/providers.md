# Provider configuration

Providers are configured in `~/.config/trouve/config.toml` (override with
`TROUVE_CONFIG`). Two kinds exist today:

- `openai-compat` — OpenAI chat completions and compatible gateways
  (OpenRouter, Ollama, vLLM, LiteLLM) via `base_url`.
- `anthropic` — the Anthropic Messages API.

Credential resolution order per provider:

1. `api_key` in the config file (discouraged; plain text)
2. `api_key_env` environment variable
3. API key in the OS keychain (`trouve auth set-key <id>`)
4. Stored OAuth tokens (`trouve auth login <id>`) when `[providers.<id>.oauth]`
   is configured — refreshed automatically via the token endpoint

Zero-config: `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` in the environment
register `openai` / `anthropic` providers automatically.

## Examples

```toml
default_model = "anthropic/claude-sonnet-4-5"

[providers.openai]
kind = "openai-compat"
api_key_env = "OPENAI_API_KEY"

[providers.anthropic]
kind = "anthropic"
# key stored via: trouve auth set-key anthropic

[providers.ollama]
kind = "openai-compat"
base_url = "http://localhost:11434/v1"
api_key = "ollama"                      # ignored by ollama but required shape

# Subscription (OAuth) auth. Device flow is used when
# device_authorization_url is set; browser PKCE otherwise.
[providers.openai-chatgpt]
kind = "openai-compat"
base_url = "https://chatgpt.com/backend-api/codex"

[providers.openai-chatgpt.oauth]
client_id = "<openai codex client id>"
authorization_url = "https://auth.openai.com/oauth/authorize"
token_url = "https://auth.openai.com/oauth/token"
scopes = ["openid", "profile", "email", "offline_access"]
redirect_port = 1455
```

Then: `trouve auth login openai-chatgpt` opens the browser flow, stores the
token set in the keychain, and the server uses (and refreshes) it as a bearer
token.

## Model catalog

`GET /v1/models` returns every model known to the configured providers, each
with a `context_window`, pricing (drives dollar-cost accounting on
`turn.completed` and the `/usage` endpoints), and an `options_schema` — a
JSON Schema describing the model's knobs (reasoning effort, thinking budget,
temperature). Clients render model option controls from that schema; no
client hardcodes per-model UI.
