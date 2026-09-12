# Provider catalog and transports

Trouve reads the provider roster and model metadata from models.dev
`api.json`. A provider is offered in Settings only when its catalog record can
be assigned to a transport below. For catalog-covered providers, live model
discovery contributes account-visible ids only; metadata and option schemas
always come from models.dev. The public catalog is downloaded and cached under
the data directory (`models-dev-cache.json`); no models.dev snapshot ships in
the binary. What is bundled is the small trouve-owned overlay
(`trouve-model-catalog.json`) whose `openai-codex` and `cursor` sections seed
those rosters until their first background refresh; overlay entries that
declare a `base_model` still resolve only once the public record they inherit
from has been downloaded. Until the first successful download the server
therefore reports `catalog_available: false`, retries every connectivity
poll, and offers only local models, overlay-only models, and trouve's own
integrations (ADR 0055).

## Supported transports

| Transport | Catalog providers | Authentication | Model discovery |
| --- | --- | --- | --- |
| OpenAI Chat Completions compatible | Catalog records using `@ai-sdk/openai-compatible`, `@ai-sdk/openai`, or OpenRouter's provider, plus documented adapters such as AIHubMix, Cerebras, Cloudflare AI Gateway, Cohere, DeepInfra, Google AI, Groq, Merge Gateway, Mistral, Perplexity, Together, v0, Venice, Vercel, and xAI | Bearer by default; template headers/query parameters can replace it | `GET /models` ids intersected with models.dev |
| Anthropic Messages | Anthropic and catalog records explicitly using its API shape | `x-api-key`, sanctioned OAuth where configured, or template auth | `GET /v1/models` ids intersected with models.dev |
| Azure OpenAI v1 | Azure OpenAI and Azure AI Services | `api-key` template header | `GET /openai/v1/models`; catalog fallback excludes Claude, which uses a Messages endpoint |
| Amazon Bedrock | Amazon Bedrock | Standard AWS credential, profile, and region chains | models.dev; ConverseStream uses the selected Bedrock model ID |
| Vertex Gemini | Google Vertex | Application Default Credentials or an explicit service-account JSON path | models.dev, filtered to the Google publisher's Gemini models |
| Anthropic on Vertex | Google Vertex Anthropic | Application Default Credentials or an explicit service-account JSON path | models.dev; requests use Vertex `streamRawPredict` with the Anthropic Messages schema |

Templated records such as Databricks, Neon, Snowflake Cortex, Cloudflare AI
Gateway, Azure, and Vertex produce setup fields from `${NAME}` placeholders.
Each field can also declare a conventional environment-variable fallback.

## Trouve-specific integrations

These records describe how Trouve executes a model; they do not duplicate the
public model catalog.

| Integration | Execution facts owned by Trouve | Model source |
| --- | --- | --- |
| Codex subscription | Codex app-server, CLI installation/login, subscription billing | `openai-codex` roster rebuilt in the background from the app-server's `model/list`, inheriting OpenAI models.dev metadata (see below) |
| Claude Code subscription | Claude CLI installation/login and command-line option mapping | Anthropic models.dev metadata |
| Cursor subscription/API key | Agent SDK Bridge lifecycle, API-key auth, and host-owned custom-tool callbacks | `cursor` roster rebuilt in the background from the Bridge's `ListModels`; public vendor models inherit models.dev metadata, Cursor-only models use the live record (see below) |
| Custom/local OpenAI-compatible endpoints | Endpoint/auth plus native Ollama or LM Studio probes where recognized | Explicit live adapter because arbitrary and user-installed ids have no catalog record |

### Vendor roster refresh

Codex and Cursor availability follow the signed-in account, so the
`openai-codex` and `cursor` sections of `trouve-model-catalog.json` are only
seeds. While online and once the public catalog is available, the engine asks
each vendor runtime for its model list in a detached task and rewrites
`<data_dir>/rosters/<provider>.json`:

- **Codex** (`model/list`): every visible model becomes an overlay patch that
  inherits name, limits and pricing from its `openai/<slug>` models.dev record
  (unknown slugs get a minimal record), live reasoning efforts and default
  replace the seed's, and `additionalSpeedTiers` yields the `fast` option.
- **Cursor** (`SdkCursorService/ListModels`, through a short-lived Bridge
  process): every model's parameters become option-schema properties keyed
  by Cursor's own parameter ids (`reasoning`, `effort`, `context`, `fast`, ...)
  with the default variant as defaults; the `context` default also sets the
  context window. Public models inherit their models.dev record when the slug
  matches a known provider; inherited reasoning options are cleared so only
  Cursor's parameter ids are ever sent.

Seed entries survive as per-model patches (Codex context limits, slug
remaps, Cursor-only metadata); models missing from the live list are dropped.
The refresh shares the models.dev cadence (one hour TTL, five minute retry
backoff) and runs at startup, on connectivity recovery, and when clients
refresh the model list. Model listings and requests never wait on a vendor
process: they read the persisted roster when it exists and the seed otherwise.

## Intentionally not exposed

| Catalog id | Reason |
| --- | --- |
| `gitlab` | The catalog package targets GitLab's internal AI integration, not a documented general-purpose model API. |
| `sap-ai-core` | SAP requires OAuth client-credential handling, resource-group headers, and discovery of a tenant-specific orchestration deployment URL. A static endpoint template would not be sufficient. |

These records are part of the downloaded models.dev catalog (and of the
models.dev test fixture; nothing is bundled into the production binary), so
adding a reviewed adapter does not require changing the catalog format. They
become visible only after the missing transport and authentication flow is
implemented.

Vendor transport replacements follow the shared
[agent backend conformance and qualification contract](agent-backend-conformance.md).
