//! Model metadata from the public, tokenless models.dev catalog.
//!
//! Provider APIs remain authoritative for availability and for any fields
//! they report. This catalog fills gaps such as context limits, pricing, and
//! model-specific reasoning controls. A generated snapshot keeps the complete
//! provider roster plus OpenAI/Anthropic model details available offline; a
//! validated disk cache is refreshed from models.dev when the server has
//! connectivity monitoring.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use reqwest::header::{ETAG, IF_NONE_MATCH};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use trouve_protocol::{KnownProvider, ModelInfo, ProviderConfigField};

const API_URL: &str = "https://models.dev/api.json";
// Version 2 retains provider metadata in addition to model records.
const CACHE_VERSION: u32 = 2;
const CATALOG_TTL: Duration = Duration::from_secs(60 * 60);
const RETRY_TTL: Duration = Duration::from_secs(5 * 60);
const SNAPSHOT: &str = include_str!("../data/models-dev-snapshot.json");

type Catalog = BTreeMap<String, CatalogProvider>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionsDialect {
    OpenAi,
    Anthropic,
    ClaudeCli,
    Gemini,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CatalogProvider {
    #[serde(default)]
    id: String,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    npm: String,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    models: BTreeMap<String, CatalogModel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CatalogModel {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tool_call: Option<bool>,
    #[serde(default)]
    temperature: Option<bool>,
    #[serde(default)]
    reasoning_options: Vec<ReasoningOption>,
    #[serde(default)]
    limit: ModelLimit,
    #[serde(default)]
    cost: ModelCost,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ReasoningOption {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default, deserialize_with = "deserialize_string_values")]
    values: Vec<String>,
    #[serde(default)]
    min: Option<i64>,
    #[serde(default)]
    max: Option<i64>,
}

fn deserialize_string_values<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Vec::<Option<String>>::deserialize(deserializer)?
        .into_iter()
        .flatten()
        .collect())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ModelLimit {
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ModelCost {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    tiers: Vec<CostTier>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CostTier {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    tier: TierRule,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TierRule {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskCache {
    version: u32,
    fetched_at: u64,
    #[serde(default)]
    etag: Option<String>,
    catalog: Catalog,
}

struct CatalogState {
    embedded: Catalog,
    remote: Option<Catalog>,
    etag: Option<String>,
    fetched_at: Option<u64>,
    last_attempt: Option<Instant>,
}

/// Shared per-engine catalog. Clones are passed to native providers and CLI
/// backends so every model-list and cost path sees the same snapshot.
pub struct ModelsDevCatalog {
    state: RwLock<CatalogState>,
    refresh_lock: tokio::sync::Mutex<()>,
    cache_path: Option<PathBuf>,
    client: reqwest::Client,
}

impl Default for ModelsDevCatalog {
    fn default() -> Self {
        Self::embedded()
    }
}

impl ModelsDevCatalog {
    /// In-memory snapshot only. Provider unit tests and standalone provider
    /// users never perform implicit network or filesystem access.
    pub fn embedded() -> Self {
        Self::from_cache_path(None)
    }

    /// Snapshot plus a last-known-good disk cache under the server data dir.
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::from_cache_path(Some(data_dir.join("models-dev-cache.json")))
    }

    fn from_cache_path(cache_path: Option<PathBuf>) -> Self {
        let embedded = parse_catalog(SNAPSHOT).expect("bundled models.dev snapshot must be valid");
        let disk = cache_path
            .as_deref()
            .and_then(|path| load_disk_cache(path).ok().flatten());
        let (remote, etag, fetched_at) = match disk {
            Some(cache) => (Some(cache.catalog), cache.etag, Some(cache.fetched_at)),
            None => (None, None, None),
        };
        let client = reqwest::Client::builder()
            .user_agent(concat!("trouve/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .expect("models.dev HTTP client configuration must be valid");
        Self {
            state: RwLock::new(CatalogState {
                embedded,
                remote,
                etag,
                fetched_at,
                last_attempt: None,
            }),
            refresh_lock: tokio::sync::Mutex::new(()),
            cache_path,
            client,
        }
    }

    /// Refresh a stale catalog. Failures preserve the in-memory and disk
    /// last-known-good data and are returned for best-effort logging.
    pub async fn refresh_if_stale(&self) -> Result<bool> {
        let _guard = self.refresh_lock.lock().await;
        {
            let state = self.state.read().unwrap();
            if state
                .last_attempt
                .is_some_and(|attempt| attempt.elapsed() < RETRY_TTL)
            {
                return Ok(false);
            }
            if state
                .fetched_at
                .is_some_and(|at| unix_now().saturating_sub(at) < CATALOG_TTL.as_secs())
            {
                return Ok(false);
            }
        }
        let etag = {
            let mut state = self.state.write().unwrap();
            state.last_attempt = Some(Instant::now());
            state.etag.clone()
        };

        let mut request = self.client.get(API_URL);
        if let Some(etag) = &etag {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request
            .send()
            .await
            .context("fetching models.dev catalog")?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            let cache = {
                let mut state = self.state.write().unwrap();
                state.fetched_at = Some(unix_now());
                state.remote.clone().map(|catalog| DiskCache {
                    version: CACHE_VERSION,
                    fetched_at: state.fetched_at.unwrap(),
                    etag: state.etag.clone(),
                    catalog,
                })
            };
            if let Some(cache) = cache {
                self.persist(&cache).await?;
            }
            return Ok(false);
        }
        let response = response.error_for_status().context("models.dev response")?;
        let response_etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        let text = response
            .text()
            .await
            .context("reading models.dev catalog")?;
        if text.len() > 16 * 1024 * 1024 {
            bail!("models.dev catalog exceeds the 16 MiB safety limit");
        }
        let catalog = parse_catalog(&text)?;
        validate_catalog(&catalog)?;
        let cache = DiskCache {
            version: CACHE_VERSION,
            fetched_at: unix_now(),
            etag: response_etag,
            catalog,
        };
        self.persist(&cache).await?;
        {
            let mut state = self.state.write().unwrap();
            state.remote = Some(cache.catalog);
            state.etag = cache.etag;
            state.fetched_at = Some(cache.fetched_at);
        }
        Ok(true)
    }

    async fn persist(&self, cache: &DiskCache) -> Result<()> {
        let Some(path) = &self.cache_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec(cache).context("serializing models.dev cache")?;
        let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        tokio::fs::write(&temp, bytes)
            .await
            .with_context(|| format!("writing {}", temp.display()))?;
        #[cfg(windows)]
        if tokio::fs::try_exists(path).await.unwrap_or(false) {
            tokio::fs::remove_file(path)
                .await
                .with_context(|| format!("replacing {}", path.display()))?;
        }
        if let Err(error) = tokio::fs::rename(&temp, path).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(error).with_context(|| format!("replacing {}", path.display()));
        }
        Ok(())
    }

    pub fn model(
        &self,
        catalog_provider: &str,
        output_provider: &str,
        model_id: &str,
        dialect: OptionsDialect,
    ) -> Option<ModelInfo> {
        let record = self.model_record(catalog_provider, model_id)?;
        (!record.is_deprecated()).then(|| record.to_model_info(output_provider, model_id, dialect))
    }

    pub fn provider_models(
        &self,
        catalog_provider: &str,
        output_provider: &str,
        dialect: OptionsDialect,
    ) -> Vec<ModelInfo> {
        let models = {
            let state = self.state.read().unwrap();
            state
                .remote
                .as_ref()
                .and_then(|catalog| provider_by_setup_id(catalog, catalog_provider))
                .or_else(|| provider_by_setup_id(&state.embedded, catalog_provider))
                .map(|provider| provider.models.clone())
                .unwrap_or_default()
        };
        models
            .into_iter()
            .filter(|(_, model)| !model.is_deprecated() && model.tool_call == Some(true))
            .map(|(id, model)| model.to_model_info(output_provider, &id, dialect))
            .collect()
    }

    /// Provider setup presets derived from the same catalog as model
    /// metadata. `api.json` supplies the roster, names, key environment
    /// variables, and explicit compatible endpoints. A small transport
    /// adapter fills endpoints for the native SDK providers Trouve already
    /// speaks through their OpenAI-compatible or Anthropic surfaces.
    pub fn provider_presets(&self) -> Vec<KnownProvider> {
        let state = self.state.read().unwrap();
        let catalog = state.remote.as_ref().unwrap_or(&state.embedded);
        let mut providers: Vec<_> = catalog
            .iter()
            .filter_map(|(catalog_id, provider)| provider.to_known_provider(catalog_id))
            .collect();
        providers.sort_by(|a, b| a.id.cmp(&b.id));
        providers
    }

    /// Match a configured endpoint back to its models.dev provider record.
    /// The suggested id wins when it names the endpoint exactly; arbitrary
    /// custom ids are matched only when the endpoint is unambiguous.
    pub fn provider_for_endpoint(
        &self,
        suggested_id: &str,
        base_url: &str,
        kind: &str,
    ) -> Option<String> {
        let state = self.state.read().unwrap();
        let catalog = state.remote.as_ref().unwrap_or(&state.embedded);
        let suggested_id = canonical_provider_id(suggested_id);
        if let Some((catalog_id, provider)) = provider_entry_by_setup_id(catalog, suggested_id)
            && provider.endpoint_matches(catalog_id, base_url, kind)
        {
            return Some(catalog_id.to_string());
        }
        let mut matches = catalog
            .iter()
            .filter(|(id, provider)| provider.endpoint_matches(id, base_url, kind))
            .map(|(id, _)| id.clone());
        let matched = matches.next()?;
        matches.next().is_none().then_some(matched)
    }

    pub fn output_limit(&self, catalog_provider: &str, model_id: &str) -> Option<u64> {
        self.model_record(catalog_provider, model_id)?
            .limit
            .output
            .filter(|value| *value > 0)
    }

    /// Calculate cost with provider-specific cache rates and context tiers
    /// from models.dev. Unknown/custom gateways retain their live list prices
    /// and conservatively bill cached input at the ordinary input rate.
    pub fn cost_usd(
        &self,
        model: &ModelInfo,
        input_tokens: u64,
        cached_input_tokens: u64,
        output_tokens: u64,
    ) -> Option<f64> {
        let base_input = model.input_price_per_mtok?;
        let base_output = model.output_price_per_mtok?;
        let (provider, model_id) = model.id.split_once('/')?;
        let record = self.model_record(provider, model_id);
        let total_input = input_tokens.saturating_add(cached_input_tokens);
        let tier = record.as_ref().and_then(|record| {
            record
                .cost
                .tiers
                .iter()
                .filter(|tier| tier.tier.kind == "context" && total_input > tier.tier.size)
                .max_by_key(|tier| tier.tier.size)
        });
        let input_price = tier
            .and_then(|tier| tier.input)
            .or_else(|| record.as_ref().and_then(|record| record.cost.input))
            .unwrap_or(base_input);
        let output_price = tier
            .and_then(|tier| tier.output)
            .or_else(|| record.as_ref().and_then(|record| record.cost.output))
            .unwrap_or(base_output);
        let cached_price = tier
            .and_then(|tier| tier.cache_read)
            .or_else(|| record.as_ref().and_then(|record| record.cost.cache_read))
            .unwrap_or(input_price);
        Some(
            (input_tokens as f64 * input_price
                + cached_input_tokens as f64 * cached_price
                + output_tokens as f64 * output_price)
                / 1_000_000.0,
        )
    }

    fn model_record(&self, provider: &str, model: &str) -> Option<CatalogModel> {
        let state = self.state.read().unwrap();
        state
            .remote
            .as_ref()
            .and_then(|catalog| provider_by_setup_id(catalog, provider))
            .and_then(|provider| provider.models.get(model))
            .or_else(|| {
                provider_by_setup_id(&state.embedded, provider)
                    .and_then(|provider| provider.models.get(model))
            })
            .cloned()
    }
}

impl CatalogModel {
    fn is_deprecated(&self) -> bool {
        self.status.as_deref() == Some("deprecated")
    }

    fn to_model_info(
        &self,
        output_provider: &str,
        fallback_id: &str,
        dialect: OptionsDialect,
    ) -> ModelInfo {
        let id = if self.id.is_empty() {
            fallback_id
        } else {
            &self.id
        };
        ModelInfo {
            id: format!("{output_provider}/{id}"),
            display_name: if self.name.is_empty() {
                id.to_string()
            } else {
                self.name.clone()
            },
            context_window: self.limit.context.unwrap_or(0),
            supports_tools: self.tool_call.unwrap_or(false),
            input_price_per_mtok: self.cost.input,
            output_price_per_mtok: self.cost.output,
            options_schema: self.options_schema(dialect),
        }
    }

    fn options_schema(&self, dialect: OptionsDialect) -> Value {
        let mut properties = Map::new();
        for option in &self.reasoning_options {
            match option.kind.as_str() {
                "effort" if option.values.len() > 1 => {
                    let key = match dialect {
                        OptionsDialect::OpenAi => "reasoning_effort",
                        OptionsDialect::Anthropic | OptionsDialect::ClaudeCli => "effort",
                        OptionsDialect::Gemini => "thinking_level",
                    };
                    properties.insert(
                        key.into(),
                        json!({
                            "type": "string",
                            "enum": option.values,
                            "description": "How much thinking the model does before answering"
                        }),
                    );
                }
                "budget_tokens"
                    if matches!(
                        dialect,
                        OptionsDialect::Anthropic
                            | OptionsDialect::ClaudeCli
                            | OptionsDialect::Gemini
                    ) =>
                {
                    let minimum = option
                        .min
                        .and_then(|value| u64::try_from(value).ok())
                        .unwrap_or(1);
                    let maximum = option
                        .max
                        .and_then(|value| u64::try_from(value).ok())
                        .or_else(|| self.limit.output.and_then(|limit| limit.checked_sub(1)));
                    let mut schema = json!({
                        "type": "integer",
                        "minimum": minimum,
                        "description": "Extended-thinking token budget; omit to disable thinking"
                    });
                    if let Some(maximum) = maximum {
                        schema["maximum"] = json!(maximum);
                    }
                    properties.insert("thinking_budget_tokens".into(), schema);
                }
                "toggle"
                    if matches!(
                        dialect,
                        OptionsDialect::Anthropic
                            | OptionsDialect::ClaudeCli
                            | OptionsDialect::Gemini
                    ) =>
                {
                    properties.insert(
                        "thinking_level".into(),
                        json!({
                            "type": "string",
                            "enum": ["off", "on"],
                            "description": "Extended thinking"
                        }),
                    );
                }
                _ => {}
            }
        }
        if self.temperature == Some(true) && dialect != OptionsDialect::ClaudeCli {
            let maximum =
                if dialect == OptionsDialect::Anthropic || dialect == OptionsDialect::Gemini {
                    1.0
                } else {
                    2.0
                };
            properties.insert(
                "temperature".into(),
                json!({"type": "number", "minimum": 0.0, "maximum": maximum}),
            );
        }
        json!({"type": "object", "properties": properties})
    }
}

impl CatalogProvider {
    fn to_known_provider(&self, catalog_id: &str) -> Option<KnownProvider> {
        let id = if self.id.is_empty() {
            catalog_id
        } else {
            &self.id
        };
        let setup_id = setup_provider_id(id);
        if setup_id.is_empty() {
            return None;
        }
        let transport = self.transport(catalog_id)?;
        let api_key_env = (transport.auth == "api-key")
            .then(|| {
                self.env.iter().find(|name| {
                    !transport
                        .config_fields
                        .iter()
                        .any(|field| field.env.as_deref() == Some(name.as_str()))
                })
            })
            .flatten();
        Some(KnownProvider {
            id: setup_id,
            display_name: if self.name.is_empty() {
                id.to_string()
            } else {
                self.name.clone()
            },
            kind: transport.kind.into(),
            base_url: transport.base_url,
            api_key_env: api_key_env.cloned(),
            config_fields: transport.config_fields,
            headers: transport.headers,
            query_params: transport.query_params,
            auth: transport.auth.into(),
            category: "api".into(),
            experimental: false,
        })
    }

    fn endpoint_matches(&self, catalog_id: &str, base_url: &str, kind: &str) -> bool {
        self.transport(catalog_id).is_some_and(|transport| {
            transport.kind == kind
                && transport.base_url.is_some_and(|endpoint| {
                    !endpoint.contains("${")
                        && normalize_endpoint(transport.kind, &endpoint)
                            == normalize_endpoint(kind, base_url)
                })
        })
    }

    fn transport(&self, catalog_id: &str) -> Option<TransportPreset> {
        let adapter = transport_adapter(catalog_id);
        let kind = if let Some(adapter) = &adapter {
            adapter.kind
        } else if self.npm == "@ai-sdk/anthropic" {
            "anthropic"
        } else if matches!(
            self.npm.as_str(),
            "@ai-sdk/openai-compatible" | "@ai-sdk/openai" | "@openrouter/ai-sdk-provider"
        ) {
            "openai-compat"
        } else {
            return None;
        };
        if let Some(adapter) = adapter {
            return Some(adapter);
        }
        let api = self.api.as_deref()?;
        Some(TransportPreset::http(kind, normalize_endpoint(kind, api)))
    }
}

#[derive(Default)]
struct TransportPreset {
    kind: &'static str,
    base_url: Option<String>,
    auth: &'static str,
    config_fields: Vec<ProviderConfigField>,
    headers: BTreeMap<String, String>,
    query_params: BTreeMap<String, String>,
}

impl TransportPreset {
    fn http(kind: &'static str, endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        Self {
            kind,
            config_fields: template_fields(&endpoint),
            base_url: Some(endpoint),
            auth: "api-key",
            ..Default::default()
        }
    }

    fn field(
        mut self,
        id: &str,
        label: &str,
        description: &str,
        env: Option<&str>,
        required: bool,
    ) -> Self {
        if let Some(field) = self.config_fields.iter_mut().find(|field| field.id == id) {
            field.label = label.into();
            field.description = description.into();
            field.env = env.map(Into::into);
            field.required = required;
        } else {
            self.config_fields.push(ProviderConfigField {
                id: id.into(),
                label: label.into(),
                description: description.into(),
                env: env.map(Into::into),
                required,
                secret: false,
                default_value: None,
            });
        }
        self
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    fn authentication(mut self, auth: &'static str) -> Self {
        self.auth = auth;
        self
    }
}

fn template_fields(template: &str) -> Vec<ProviderConfigField> {
    let mut fields = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find('}') else { break };
        let name = &rest[..end];
        if !name.is_empty()
            && !fields
                .iter()
                .any(|field: &ProviderConfigField| field.id == name)
        {
            fields.push(ProviderConfigField {
                id: name.into(),
                label: name.replace('_', " "),
                description: String::new(),
                env: Some(name.into()),
                required: true,
                secret: false,
                default_value: None,
            });
        }
        rest = &rest[end + 1..];
    }
    fields
}

fn canonical_provider_id(id: &str) -> &str {
    match id {
        // Backward-compatible aliases for presets shipped before the
        // models.dev provider roster became authoritative.
        "gemini" => "google",
        "kilocode" => "kilo",
        "together" => "togetherai",
        "moonshot" => "moonshotai",
        other => other,
    }
}

fn setup_provider_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn provider_entry_by_setup_id<'a>(
    catalog: &'a Catalog,
    requested: &str,
) -> Option<(&'a str, &'a CatalogProvider)> {
    let canonical = canonical_provider_id(requested);
    catalog
        .get_key_value(canonical)
        .map(|(id, provider)| (id.as_str(), provider))
        .or_else(|| {
            catalog.iter().find_map(|(catalog_id, provider)| {
                let source_id = if provider.id.is_empty() {
                    catalog_id
                } else {
                    &provider.id
                };
                (setup_provider_id(source_id) == requested)
                    .then_some((catalog_id.as_str(), provider))
            })
        })
}

fn provider_by_setup_id<'a>(catalog: &'a Catalog, requested: &str) -> Option<&'a CatalogProvider> {
    provider_entry_by_setup_id(catalog, requested).map(|(_, provider)| provider)
}

/// API endpoints omitted by models.dev because those records target a native
/// JavaScript SDK. Trouve's transport is different, so these are integration
/// adapters rather than catalog records; all roster/name/env data still comes
/// from api.json.
fn transport_adapter(provider: &str) -> Option<TransportPreset> {
    Some(match provider {
        "openai" => TransportPreset::http("openai-compat", "https://api.openai.com/v1"),
        "anthropic" => TransportPreset::http("anthropic", "https://api.anthropic.com"),
        "google" => TransportPreset::http(
            "openai-compat",
            "https://generativelanguage.googleapis.com/v1beta/openai",
        ),
        "xai" => TransportPreset::http("openai-compat", "https://api.x.ai/v1"),
        "groq" => TransportPreset::http("openai-compat", "https://api.groq.com/openai/v1"),
        "mistral" => TransportPreset::http("openai-compat", "https://api.mistral.ai/v1"),
        "perplexity" => TransportPreset::http("openai-compat", "https://api.perplexity.ai"),
        "togetherai" => TransportPreset::http("openai-compat", "https://api.together.xyz/v1"),
        "cohere" => TransportPreset::http("openai-compat", "https://api.cohere.ai/compatibility/v1"),
        // Native AI SDK packages that also publish a documented OpenAI Chat
        // Completions surface. The wire contract, not the npm package name,
        // is what makes these share the adapter.
        "cerebras" => TransportPreset::http("openai-compat", "https://api.cerebras.ai/v1"),
        "deepinfra" => TransportPreset::http("openai-compat", "https://api.deepinfra.com/v1/openai"),
        "venice" => TransportPreset::http("openai-compat", "https://api.venice.ai/api/v1"),
        "vercel" => TransportPreset::http("openai-compat", "https://ai-gateway.vercel.sh/v1"),
        "v0" => TransportPreset::http("openai-compat", "https://api.v0.dev/v1"),
        "aihubmix" => TransportPreset::http("openai-compat", "https://aihubmix.com/v1"),
        "merge-gateway" => TransportPreset::http(
            "openai-compat",
            "https://api-gateway.merge.dev/v1/openai",
        ),
        "cloudflare-ai-gateway" => TransportPreset::http(
            "openai-compat",
            "https://gateway.ai.cloudflare.com/v1/${CLOUDFLARE_ACCOUNT_ID}/${CLOUDFLARE_GATEWAY_ID}/ai/v1",
        )
        .field(
            "CLOUDFLARE_ACCOUNT_ID",
            "Cloudflare account ID",
            "Account that owns the AI Gateway",
            Some("CLOUDFLARE_ACCOUNT_ID"),
            true,
        )
        .field(
            "CLOUDFLARE_GATEWAY_ID",
            "Cloudflare gateway ID",
            "Name of the AI Gateway",
            Some("CLOUDFLARE_GATEWAY_ID"),
            true,
        ),
        "azure" => TransportPreset::http(
            "azure-openai",
            "https://${AZURE_RESOURCE_NAME}.openai.azure.com/openai/v1",
        )
        .field(
            "AZURE_RESOURCE_NAME",
            "Azure resource name",
            "The subdomain of the Azure OpenAI resource",
            Some("AZURE_RESOURCE_NAME"),
            true,
        )
        .header("api-key", "${API_KEY}"),
        "azure-cognitive-services" => TransportPreset::http(
            "azure-openai",
            "https://${AZURE_COGNITIVE_SERVICES_RESOURCE_NAME}.services.ai.azure.com/openai/v1",
        )
        .field(
            "AZURE_COGNITIVE_SERVICES_RESOURCE_NAME",
            "Azure resource name",
            "The subdomain of the Azure AI Services resource",
            Some("AZURE_COGNITIVE_SERVICES_RESOURCE_NAME"),
            true,
        )
        .header("api-key", "${API_KEY}"),
        "amazon-bedrock" => TransportPreset {
            kind: "amazon-bedrock",
            auth: "aws",
            ..Default::default()
        }
        .field(
            "AWS_REGION",
            "AWS region",
            "Region containing the Bedrock models to use",
            Some("AWS_REGION"),
            true,
        )
        .field(
            "AWS_PROFILE",
            "AWS profile",
            "Optional shared-config profile; the default credential chain is used when omitted",
            Some("AWS_PROFILE"),
            false,
        ),
        "google-vertex" => TransportPreset::http(
            "google-vertex",
        "https://${GOOGLE_VERTEX_LOCATION}-aiplatform.googleapis.com/v1/projects/${GOOGLE_VERTEX_PROJECT}/locations/${GOOGLE_VERTEX_LOCATION}/publishers/google",
        )
        .authentication("gcp")
        .field(
            "GOOGLE_VERTEX_PROJECT",
            "Google Cloud project",
            "Project ID that owns the Vertex AI models",
            Some("GOOGLE_CLOUD_PROJECT"),
            true,
        )
        .field(
            "GOOGLE_VERTEX_LOCATION",
            "Vertex location",
            "Regional Vertex AI location, such as us-central1",
            Some("GOOGLE_CLOUD_LOCATION"),
            true,
        )
        .field(
            "GOOGLE_APPLICATION_CREDENTIALS",
            "Application credentials file",
            "Optional service-account JSON path; otherwise Application Default Credentials are used",
            Some("GOOGLE_APPLICATION_CREDENTIALS"),
            false,
        ),
        "google-vertex-anthropic" => TransportPreset::http(
            "google-vertex-anthropic",
            "https://${GOOGLE_VERTEX_LOCATION}-aiplatform.googleapis.com/v1/projects/${GOOGLE_VERTEX_PROJECT}/locations/${GOOGLE_VERTEX_LOCATION}/publishers/anthropic",
        )
        .authentication("gcp")
        .field(
            "GOOGLE_VERTEX_PROJECT",
            "Google Cloud project",
            "Project ID that owns the Vertex AI models",
            Some("GOOGLE_CLOUD_PROJECT"),
            true,
        )
        .field(
            "GOOGLE_VERTEX_LOCATION",
            "Vertex location",
            "Regional Vertex AI location, such as us-east5",
            Some("GOOGLE_CLOUD_LOCATION"),
            true,
        )
        .field(
            "GOOGLE_APPLICATION_CREDENTIALS",
            "Application credentials file",
            "Optional service-account JSON path; otherwise Application Default Credentials are used",
            Some("GOOGLE_APPLICATION_CREDENTIALS"),
            false,
        ),
        _ => return None,
    })
}

fn normalize_endpoint(kind: &str, endpoint: &str) -> String {
    let mut endpoint = endpoint.trim_end_matches('/');
    if kind == "anthropic" {
        endpoint = endpoint.strip_suffix("/v1").unwrap_or(endpoint);
    } else {
        endpoint = endpoint
            .strip_suffix("/chat/completions")
            .unwrap_or(endpoint)
            .trim_end_matches('/');
    }
    endpoint.to_string()
}

fn parse_catalog(text: &str) -> Result<Catalog> {
    serde_json::from_str(text).context("parsing models.dev catalog")
}

fn validate_catalog(catalog: &Catalog) -> Result<()> {
    if catalog.is_empty() || catalog.values().all(|provider| provider.models.is_empty()) {
        bail!("models.dev catalog contains no models");
    }
    Ok(())
}

fn load_disk_cache(path: &Path) -> Result<Option<DiskCache>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let cache: DiskCache =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if cache.version != CACHE_VERSION {
        return Ok(None);
    }
    validate_catalog(&cache.catalog)?;
    Ok(Some(cache))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_has_current_gpt_and_fable_metadata() {
        let catalog = ModelsDevCatalog::embedded();
        let gpt = catalog
            .model("openai", "openai", "gpt-5.6", OptionsDialect::OpenAi)
            .unwrap();
        assert_eq!(gpt.context_window, 1_050_000);
        assert_eq!(gpt.input_price_per_mtok, Some(5.0));
        assert_eq!(
            gpt.options_schema
                .pointer("/properties/reasoning_effort/enum")
                .unwrap(),
            &json!(["none", "low", "medium", "high", "xhigh", "max"])
        );

        let fable = catalog
            .model(
                "anthropic",
                "claude-code",
                "claude-fable-5",
                OptionsDialect::ClaudeCli,
            )
            .unwrap();
        assert_eq!(
            fable
                .options_schema
                .pointer("/properties/effort/enum")
                .unwrap(),
            &json!(["low", "medium", "high", "xhigh", "max"])
        );
    }

    #[test]
    fn fixed_thinking_is_a_numeric_catalog_bound_not_invented_levels() {
        let catalog = ModelsDevCatalog::embedded();
        let model = catalog
            .model(
                "anthropic",
                "anthropic",
                "claude-haiku-4-5",
                OptionsDialect::Anthropic,
            )
            .unwrap();
        assert_eq!(
            model
                .options_schema
                .pointer("/properties/thinking_budget_tokens/minimum"),
            Some(&json!(1024))
        );
        assert!(
            model
                .options_schema
                .pointer("/properties/thinking_level")
                .is_none()
        );
    }

    #[test]
    fn catalog_drives_cache_and_long_context_pricing() {
        let catalog = ModelsDevCatalog::embedded();
        let model = catalog
            .model("openai", "openai", "gpt-5.6", OptionsDialect::OpenAi)
            .unwrap();
        let cost = catalog.cost_usd(&model, 200_000, 100_000, 10_000).unwrap();
        // Long-context tier: $10/M ordinary input, $1/M cached, $45/M output.
        assert!((cost - 2.55).abs() < 1e-10, "cost was {cost}");
    }

    #[test]
    fn snapshot_drives_provider_setup_catalog() {
        let catalog = ModelsDevCatalog::embedded();
        let providers = catalog.provider_presets();
        assert!(providers.len() >= 145, "only {} providers", providers.len());

        let openrouter = providers
            .iter()
            .find(|provider| provider.id == "openrouter")
            .unwrap();
        assert_eq!(openrouter.display_name, "OpenRouter");
        assert_eq!(
            openrouter.base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            openrouter.api_key_env.as_deref(),
            Some("OPENROUTER_API_KEY")
        );

        // Google has no `api` field because models.dev targets its native SDK;
        // the roster/name/env remain catalog data and Trouve supplies only the
        // compatible transport endpoint.
        let google = providers
            .iter()
            .find(|provider| provider.id == "google")
            .unwrap();
        assert_eq!(google.display_name, "Google");
        assert_eq!(google.api_key_env.as_deref(), Some("GOOGLE_API_KEY"));
        assert_eq!(
            google.base_url.as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/openai")
        );

        let minimax = providers
            .iter()
            .find(|provider| provider.id == "minimax")
            .unwrap();
        assert_eq!(minimax.kind, "anthropic");
        assert_eq!(
            minimax.base_url.as_deref(),
            Some("https://api.minimax.io/anthropic")
        );
        let databricks = providers
            .iter()
            .find(|provider| provider.id == "databricks")
            .unwrap();
        assert_eq!(databricks.kind, "openai-compat");
        assert!(
            databricks
                .base_url
                .as_deref()
                .unwrap()
                .contains("${DATABRICKS_HOST}")
        );
        assert!(
            databricks
                .config_fields
                .iter()
                .any(|field| field.id == "DATABRICKS_HOST")
        );

        let azure = providers
            .iter()
            .find(|provider| provider.id == "azure")
            .unwrap();
        assert_eq!(azure.kind, "azure-openai");
        assert_eq!(azure.auth, "api-key");
        assert_eq!(
            azure.headers.get("api-key").map(String::as_str),
            Some("${API_KEY}")
        );

        let bedrock = providers
            .iter()
            .find(|provider| provider.id == "amazon-bedrock")
            .unwrap();
        assert_eq!(bedrock.kind, "amazon-bedrock");
        assert_eq!(bedrock.auth, "aws");
        assert!(bedrock.api_key_env.is_none());

        let vertex = providers
            .iter()
            .find(|provider| provider.id == "google-vertex")
            .unwrap();
        assert_eq!(vertex.kind, "google-vertex");
        assert_eq!(vertex.auth, "gcp");
        assert!(vertex.api_key_env.is_none());

        let vertex_anthropic = providers
            .iter()
            .find(|provider| provider.id == "google-vertex-anthropic")
            .unwrap();
        assert_eq!(vertex_anthropic.kind, "google-vertex-anthropic");
        assert_eq!(vertex_anthropic.auth, "gcp");

        for id in ["aihubmix", "merge-gateway"] {
            let provider = providers.iter().find(|provider| provider.id == id).unwrap();
            assert_eq!(provider.kind, "openai-compat");
            assert!(provider.base_url.is_some());
        }
    }

    #[test]
    fn endpoint_matching_uses_catalog_and_preserves_old_aliases() {
        let catalog = ModelsDevCatalog::embedded();
        assert_eq!(
            catalog.provider_for_endpoint(
                "custom",
                "https://openrouter.ai/api/v1/",
                "openai-compat"
            ),
            Some("openrouter".into())
        );
        assert_eq!(
            catalog.provider_for_endpoint(
                "gemini",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "openai-compat"
            ),
            Some("google".into())
        );
    }

    #[test]
    fn unsupported_catalog_transports_are_explicitly_triaged() {
        let catalog = parse_catalog(SNAPSHOT).unwrap();
        let unsupported: Vec<_> = catalog
            .iter()
            .filter(|(id, provider)| provider.to_known_provider(id).is_none())
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(unsupported, ["gitlab", "sap-ai-core"]);
    }

    #[test]
    fn valid_disk_cache_overlays_the_embedded_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models-dev-cache.json");
        let remote = parse_catalog(
            r#"{"openai":{"models":{"future":{"id":"future","name":"Future","tool_call":true,"limit":{"context":42}}}}}"#,
        )
        .unwrap();
        let cache = DiskCache {
            version: CACHE_VERSION,
            fetched_at: unix_now(),
            etag: Some("test".into()),
            catalog: remote,
        };
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
        let catalog = ModelsDevCatalog::from_cache_path(Some(path));
        assert_eq!(
            catalog
                .model("openai", "openai", "future", OptionsDialect::OpenAi)
                .unwrap()
                .context_window,
            42
        );
        assert!(
            catalog
                .model("openai", "openai", "gpt-5.6", OptionsDialect::OpenAi)
                .is_some()
        );
    }
}
