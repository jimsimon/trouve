# Provider catalog and transports

Trouve reads the provider roster and model metadata from models.dev
`api.json`. A provider is offered in Settings only when its catalog record can
be assigned to a transport below. Live model discovery is preferred when the
service exposes it; the full generated snapshot supplies offline metadata and
fallback model lists.

## Supported transports

| Transport | Catalog providers | Authentication | Model discovery |
| --- | --- | --- | --- |
| OpenAI Chat Completions compatible | Catalog records using `@ai-sdk/openai-compatible`, `@ai-sdk/openai`, or OpenRouter's provider, plus documented adapters such as AIHubMix, Cerebras, Cloudflare AI Gateway, Cohere, DeepInfra, Google AI, Groq, Merge Gateway, Mistral, Perplexity, Together, v0, Venice, Vercel, and xAI | Bearer by default; template headers/query parameters can replace it | `GET /models`, enriched from models.dev |
| Anthropic Messages | Anthropic and catalog records explicitly using its API shape | `x-api-key`, sanctioned OAuth where configured, or template auth | `GET /v1/models`, then models.dev fallback |
| Azure OpenAI v1 | Azure OpenAI and Azure AI Services | `api-key` template header | `GET /openai/v1/models`; catalog fallback excludes Claude, which uses a Messages endpoint |
| Amazon Bedrock | Amazon Bedrock | Standard AWS credential, profile, and region chains | models.dev; ConverseStream uses the selected Bedrock model ID |
| Vertex Gemini | Google Vertex | Application Default Credentials or an explicit service-account JSON path | models.dev, filtered to the Google publisher's Gemini models |
| Anthropic on Vertex | Google Vertex Anthropic | Application Default Credentials or an explicit service-account JSON path | models.dev; requests use Vertex `streamRawPredict` with the Anthropic Messages schema |

Templated records such as Databricks, Neon, Snowflake Cortex, Cloudflare AI
Gateway, Azure, and Vertex produce setup fields from `${NAME}` placeholders.
Each field can also declare a conventional environment-variable fallback.

## Intentionally not exposed

| Catalog id | Reason |
| --- | --- |
| `gitlab` | The catalog package targets GitLab's internal AI integration, not a documented general-purpose model API. |
| `sap-ai-core` | SAP requires OAuth client-credential handling, resource-group headers, and discovery of a tenant-specific orchestration deployment URL. A static endpoint template would not be sufficient. |

These records remain in the bundled catalog so adding a reviewed adapter does
not require changing the snapshot format. They become visible only after the
missing transport and authentication flow is implemented.
