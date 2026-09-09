//! Model metadata from the public, tokenless models.dev catalog.
//!
//! Provider APIs may contribute account-specific availability, while this
//! catalog remains authoritative for provider identity, model metadata, model
//! rosters, and model-specific option schemas. The catalog is downloaded and
//! cached on disk (`models-dev-cache.json`, refreshed on a TTL with ETag
//! revalidation while the server has connectivity monitoring); nothing is
//! bundled into the binary, so until the first successful download the
//! catalog is empty and callers should surface that state rather than guess.
//! A small trouve-owned overlay describes serving surfaces that models.dev
//! does not distinguish (Codex, Cursor) and fills in facts the vendors do not
//! report. Those providers persist a per-install roster file under
//! `<data_dir>/rosters/` that the backend rebuilds in the background from its
//! live model list; when present it replaces the bundled seed for that
//! provider so retired models stop being offered without a trouve release.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use futures::StreamExt as _;
use reqwest::header::{ETAG, IF_NONE_MATCH};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use trouve_protocol::{KnownProvider, ModelInfo, ProviderConfigField};

const API_URL: &str = "https://models.dev/api.json";
// Version 2 retains provider metadata in addition to model records.
const CACHE_VERSION: u32 = 2;
const ROSTER_VERSION: u32 = 1;
const CATALOG_TTL: Duration = Duration::from_secs(60 * 60);
const RETRY_TTL: Duration = Duration::from_secs(5 * 60);
/// Without any catalog at all nothing works, so retry much sooner than the
/// steady-state backoff.
const EMPTY_RETRY_TTL: Duration = Duration::from_secs(30);
/// A full models.dev copy used only as test data; it never ships in release
/// binaries.
#[cfg(any(test, feature = "catalog-fixture"))]
const SNAPSHOT: &str = include_str!("../data/models-dev-snapshot.json");
const TROUVE_CATALOG: &str = include_str!("../data/trouve-model-catalog.json");

type Catalog = BTreeMap<String, CatalogProvider>;
type CatalogOverlay = BTreeMap<String, CatalogOverlayProvider>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionsDialect {
    OpenAi,
    CodexCli,
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

/// A trouve-owned provider record has the same provider/model nesting as
/// models.dev's `api.json`, but model bodies stay as JSON until lookup so a
/// `base_model` can inherit from the latest remote (or embedded) catalog.
#[derive(Debug, Clone, Default, Deserialize)]
struct CatalogOverlayProvider {
    #[serde(default)]
    id: String,
    #[serde(default)]
    models: BTreeMap<String, Value>,
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
    attachment: Option<bool>,
    #[serde(default)]
    temperature: Option<bool>,
    #[serde(default)]
    reasoning_options: Vec<ReasoningOption>,
    /// Trouve-owned serving surfaces may advertise transport-specific scalar
    /// controls that models.dev does not describe (for example Fast mode).
    /// Each value is the JSON Schema property for that option.
    #[serde(default)]
    options: Map<String, Value>,
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
    /// Trouve-owned catalogs may pin a serving surface's actual default. The
    /// public `api.json` currently omits this field, so upstream records retain
    /// the conventional midpoint fallback.
    #[serde(default)]
    default: Option<String>,
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
    input: Option<u64>,
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

/// Per-install roster for one overlay provider, rebuilt in the background
/// from a live vendor model list. Model bodies use the same overlay patch
/// schema as the bundled trouve catalog (`base_model`, `reasoning_options`,
/// `options`, `limit`, ...) so they resolve through `resolve_overlay_model`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RosterFile {
    version: u32,
    fetched_at: u64,
    models: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default)]
struct RosterState {
    models: BTreeMap<String, Value>,
    fetched_at: Option<u64>,
    last_attempt: Option<Instant>,
}

struct CatalogState {
    owned: CatalogOverlay,
    /// Refreshed rosters keyed by canonical overlay provider id. An entry
    /// replaces the bundled `owned` models for that provider wholesale.
    rosters: BTreeMap<String, RosterState>,
    /// The last downloaded models.dev catalog; `None` until the first
    /// successful fetch (or valid disk cache).
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
    rosters_dir: Option<PathBuf>,
    client: reqwest::Client,
}

impl Default for ModelsDevCatalog {
    fn default() -> Self {
        Self::empty()
    }
}

impl ModelsDevCatalog {
    /// No public catalog and no filesystem: standalone provider users and
    /// backends constructed before the engine hands them the shared catalog.
    /// Overlay-only models (no `base_model`) still resolve.
    pub fn empty() -> Self {
        Self::from_paths(None, None)
    }

    /// The bundled test copy of models.dev as the "downloaded" catalog. Tests
    /// only: never performs network or filesystem access.
    #[cfg(any(test, feature = "catalog-fixture"))]
    pub fn fixture() -> Self {
        Self::from_paths(None, None).with_fixture_catalog()
    }

    /// `for_data_dir` seeded with the test copy of models.dev whenever the
    /// data dir holds no valid cache. Tests only.
    #[cfg(any(test, feature = "catalog-fixture"))]
    pub fn fixture_for_data_dir(data_dir: &Path) -> Self {
        Self::for_data_dir(data_dir).with_fixture_catalog()
    }

    #[cfg(any(test, feature = "catalog-fixture"))]
    fn with_fixture_catalog(self) -> Self {
        {
            let mut state = self.state.write().unwrap();
            if state.remote.is_none() {
                state.remote =
                    Some(parse_catalog(SNAPSHOT).expect("test models.dev snapshot must be valid"));
            }
        }
        self
    }

    /// Write the test copy of models.dev as a valid disk cache under
    /// `data_dir`, so an engine built from that directory starts with the
    /// public catalog without any network access. Tests only.
    #[cfg(any(test, feature = "catalog-fixture"))]
    pub fn write_fixture_cache(data_dir: &Path) -> Result<()> {
        let catalog = parse_catalog(SNAPSHOT).expect("test models.dev snapshot must be valid");
        let cache = DiskCache {
            version: CACHE_VERSION,
            fetched_at: unix_now(),
            etag: None,
            catalog,
        };
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating {}", data_dir.display()))?;
        let path = data_dir.join("models-dev-cache.json");
        std::fs::write(&path, serde_json::to_vec(&cache)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Last-known-good disk cache and rosters under the server data dir.
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self::from_paths(
            Some(data_dir.join("models-dev-cache.json")),
            Some(data_dir.join("rosters")),
        )
    }

    fn from_paths(cache_path: Option<PathBuf>, rosters_dir: Option<PathBuf>) -> Self {
        let owned = parse_catalog_overlay(TROUVE_CATALOG)
            .expect("bundled trouve model catalog must be valid");
        let disk = cache_path
            .as_deref()
            .and_then(|path| load_disk_cache(path).ok().flatten());
        let (remote, etag, fetched_at) = match disk {
            Some(cache) => (Some(cache.catalog), cache.etag, Some(cache.fetched_at)),
            None => (None, None, None),
        };
        let rosters = rosters_dir.as_deref().map(load_rosters).unwrap_or_default();
        let client = reqwest::Client::builder()
            .user_agent(concat!("trouve/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .expect("models.dev HTTP client configuration must be valid");
        Self {
            state: RwLock::new(CatalogState {
                owned,
                rosters,
                remote,
                etag,
                fetched_at,
                last_attempt: None,
            }),
            refresh_lock: tokio::sync::Mutex::new(()),
            cache_path,
            rosters_dir,
            client,
        }
    }

    /// Whether a public catalog has been downloaded (or restored from disk).
    /// While false, only overlay-only models exist and setup presets are
    /// limited to trouve's own integrations.
    pub fn is_available(&self) -> bool {
        self.state.read().unwrap().remote.is_some()
    }

    /// Refresh a stale catalog. Failures preserve the in-memory and disk
    /// last-known-good data and are returned for best-effort logging.
    pub async fn refresh_if_stale(&self) -> Result<bool> {
        let _guard = self.refresh_lock.lock().await;
        {
            let state = self.state.read().unwrap();
            let retry_ttl = if state.remote.is_some() {
                RETRY_TTL
            } else {
                EMPTY_RETRY_TTL
            };
            if state
                .last_attempt
                .is_some_and(|attempt| attempt.elapsed() < retry_ttl)
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
        const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
        {
            bail!("models.dev catalog exceeds the 16 MiB safety limit");
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading models.dev catalog")?;
            if body
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > MAX_CATALOG_BYTES)
            {
                bail!("models.dev catalog exceeds the 16 MiB safety limit");
            }
            body.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(body).context("reading models.dev catalog")?;
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
        let bytes = serde_json::to_vec(cache).context("serializing models.dev cache")?;
        write_json_atomically(path, bytes).await
    }

    /// The overlay patches currently in effect for a trouve-owned provider:
    /// the refreshed roster when one exists, otherwise the bundled seed. A
    /// roster rebuild starts from this so hand-authored extras (transport
    /// options, context limits) survive refreshes.
    pub fn owned_provider_models(&self, provider: &str) -> BTreeMap<String, Value> {
        let state = self.state.read().unwrap();
        owned_provider_models(&state, provider)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `provider/model` exists in the public (remote or embedded)
    /// catalog, i.e. whether an overlay patch may inherit from it via
    /// `base_model`.
    pub fn has_source_model(&self, provider: &str, model: &str) -> bool {
        let state = self.state.read().unwrap();
        source_model(&state, provider, model).is_some()
    }

    /// Same TTL and retry backoff as the models.dev refresh, tracked per
    /// overlay provider. When a refresh is due this also records the attempt
    /// under the same lock, so overlapping triggers admit exactly one live
    /// lookup per retry window.
    pub fn begin_roster_refresh(&self, provider: &str) -> bool {
        let mut state = self.state.write().unwrap();
        let roster = state
            .rosters
            .entry(canonical_provider_id(provider).to_string())
            .or_default();
        if roster
            .last_attempt
            .is_some_and(|attempt| attempt.elapsed() < RETRY_TTL)
        {
            return false;
        }
        if roster
            .fetched_at
            .is_some_and(|at| unix_now().saturating_sub(at) < CATALOG_TTL.as_secs())
        {
            return false;
        }
        roster.last_attempt = Some(Instant::now());
        true
    }

    /// Whether `begin_roster_refresh` would admit a refresh (status, tests).
    pub fn roster_needs_refresh(&self, provider: &str) -> bool {
        let state = self.state.read().unwrap();
        let Some(roster) = state.rosters.get(canonical_provider_id(provider)) else {
            return true;
        };
        if roster
            .last_attempt
            .is_some_and(|attempt| attempt.elapsed() < RETRY_TTL)
        {
            return false;
        }
        !roster
            .fetched_at
            .is_some_and(|at| unix_now().saturating_sub(at) < CATALOG_TTL.as_secs())
    }

    /// Replace a provider's roster in memory and on disk. An empty roster is
    /// rejected so a bad live response never empties the model picker.
    pub async fn replace_roster(
        &self,
        provider: &str,
        models: BTreeMap<String, Value>,
    ) -> Result<()> {
        if models.is_empty() {
            bail!("{provider} roster contains no models");
        }
        let provider = canonical_provider_id(provider).to_string();
        let file = RosterFile {
            version: ROSTER_VERSION,
            fetched_at: unix_now(),
            models,
        };
        if let Some(dir) = &self.rosters_dir {
            let bytes = serde_json::to_vec_pretty(&file)
                .with_context(|| format!("serializing {provider} roster"))?;
            write_json_atomically(&dir.join(format!("{provider}.json")), bytes).await?;
        }
        let mut state = self.state.write().unwrap();
        let roster = state.rosters.entry(provider).or_default();
        roster.models = file.models;
        roster.fetched_at = Some(file.fetched_at);
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

    /// Return the catalog identity used to group equivalent serving routes.
    ///
    /// Public catalog entries use their canonical model id. Trouve-owned
    /// serving-surface overlays participate only when they explicitly inherit
    /// a public `base_model`; transport-owned entries without that link remain
    /// concrete-only even when their display id happens to match another
    /// provider's model.
    pub fn shared_model_identity(&self, catalog_provider: &str, model_id: &str) -> Option<String> {
        let state = self.state.read().unwrap();
        if let Some(patch) = overlay_provider_by_setup_id(&state.owned, catalog_provider)
            .and_then(|provider| provider.models.get(model_id))
        {
            let base = patch.as_object()?.get("base_model")?.as_str()?;
            let (provider, source_id) = base.split_once('/')?;
            let record = source_model(&state, provider, source_id)?;
            return runnable_shared_identity(record, source_id);
        }
        let record = source_model(&state, catalog_provider, model_id)?;
        runnable_shared_identity(record, model_id)
    }

    pub fn provider_models(
        &self,
        catalog_provider: &str,
        output_provider: &str,
        dialect: OptionsDialect,
    ) -> Vec<ModelInfo> {
        let models = {
            let state = self.state.read().unwrap();
            let mut models = source_provider(&state, catalog_provider)
                .map(|provider| provider.models.clone())
                .unwrap_or_default();
            if let Some(patches) = owned_provider_models(&state, catalog_provider) {
                for (id, patch) in patches {
                    if let Some(model) = resolve_overlay_model(&state, id, patch) {
                        models.insert(id.clone(), model);
                    }
                }
            }
            models
        };
        models
            .into_iter()
            .filter(|(_, model)| !model.is_deprecated() && model.tool_call == Some(true))
            .map(|(id, model)| model.to_model_info(output_provider, &id, dialect))
            .collect()
    }

    /// Catalog-owned metadata for the account-visible ids reported by a live
    /// provider or vendor CLI. The live source contributes only availability:
    /// unknown, deprecated, and non-tool models are omitted rather than
    /// synthesizing metadata from a partial vendor record.
    pub fn provider_models_for_ids<I, S>(
        &self,
        catalog_provider: &str,
        output_provider: &str,
        available_ids: I,
        dialect: OptionsDialect,
    ) -> Vec<ModelInfo>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut models = BTreeMap::new();
        for id in available_ids {
            let Some(model) = self.model(catalog_provider, output_provider, id.as_ref(), dialect)
            else {
                continue;
            };
            if model.supports_tools {
                models.entry(model.id.clone()).or_insert(model);
            }
        }
        models.into_values().collect()
    }

    /// Provider setup presets derived from the same catalog as model
    /// metadata. `api.json` supplies the roster, names, key environment
    /// variables, and explicit compatible endpoints. A small transport
    /// adapter fills endpoints for the native SDK providers Trouve already
    /// speaks through their OpenAI-compatible or Anthropic surfaces.
    pub fn provider_presets(&self) -> Vec<KnownProvider> {
        let state = self.state.read().unwrap();
        let Some(catalog) = state.remote.as_ref() else {
            return Vec::new();
        };
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
        let catalog = state.remote.as_ref()?;
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
        owned_provider_models(&state, provider)
            .and_then(|models| models.get(model))
            .and_then(|patch| resolve_overlay_model(&state, model, patch))
            .or_else(|| source_model(&state, provider, model).cloned())
    }
}

/// Write `bytes` to `path` via a same-directory temp file and rename so a
/// crash never leaves a truncated cache behind. Every call gets its own
/// temp file, so concurrent writers of one destination cannot clobber each
/// other mid-write; the last rename wins. `rename` replaces an existing
/// destination on every supported platform (Windows uses
/// MOVEFILE_REPLACE_EXISTING), so the last-known-good file is never absent.
async fn write_json_atomically(path: &Path, bytes: Vec<u8>) -> Result<()> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let temp = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    tokio::fs::write(&temp, bytes)
        .await
        .with_context(|| format!("writing {}", temp.display()))?;
    if let Err(error) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
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
            supports_images: self.attachment.unwrap_or(false),
            input_price_per_mtok: self.cost.input,
            output_price_per_mtok: self.cost.output,
            options_schema: self.options_schema(dialect),
        }
    }

    fn options_schema(&self, dialect: OptionsDialect) -> Value {
        let mut properties = self.options.clone();
        for option in &self.reasoning_options {
            match option.kind.as_str() {
                "effort" if option.values.len() > 1 => {
                    let key = match dialect {
                        OptionsDialect::OpenAi | OptionsDialect::CodexCli => "reasoning_effort",
                        OptionsDialect::Anthropic | OptionsDialect::ClaudeCli => "effort",
                        OptionsDialect::Gemini => "thinking_level",
                    };
                    let mut schema = json!({
                        "type": "string",
                        "enum": option.values,
                        "description": "How much thinking the model does before answering"
                    });
                    // Public models.dev records supply the supported ordering
                    // but not a separate default. Trouve-owned serving-surface
                    // records can pin one; otherwise prefer the conventional
                    // midpoint, then the catalog's first supported value.
                    if let Some(default) = option
                        .default
                        .as_ref()
                        .filter(|default| option.values.contains(default))
                        .or_else(|| option.values.iter().find(|value| *value == "medium"))
                        .or_else(|| option.values.first())
                    {
                        schema["default"] = json!(default);
                    }
                    properties.insert(key.into(), schema);
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
                            "default": "off",
                            "description": "Extended thinking"
                        }),
                    );
                }
                _ => {}
            }
        }
        if self.temperature == Some(true)
            && !matches!(
                dialect,
                OptionsDialect::CodexCli | OptionsDialect::ClaudeCli
            )
        {
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

fn runnable_shared_identity(record: &CatalogModel, fallback_id: &str) -> Option<String> {
    (!record.is_deprecated() && record.tool_call == Some(true)).then(|| {
        if record.id.is_empty() {
            fallback_id.to_string()
        } else {
            record.id.clone()
        }
    })
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

fn source_provider<'a>(state: &'a CatalogState, requested: &str) -> Option<&'a CatalogProvider> {
    state
        .remote
        .as_ref()
        .and_then(|catalog| provider_by_setup_id(catalog, requested))
}

fn source_model<'a>(
    state: &'a CatalogState,
    provider: &str,
    model: &str,
) -> Option<&'a CatalogModel> {
    state
        .remote
        .as_ref()
        .and_then(|catalog| provider_by_setup_id(catalog, provider))
        .and_then(|provider| provider.models.get(model))
}

fn overlay_provider_by_setup_id<'a>(
    catalog: &'a CatalogOverlay,
    requested: &str,
) -> Option<&'a CatalogOverlayProvider> {
    let canonical = canonical_provider_id(requested);
    catalog.get(canonical).or_else(|| {
        catalog.iter().find_map(|(catalog_id, provider)| {
            let source_id = if provider.id.is_empty() {
                catalog_id
            } else {
                &provider.id
            };
            (setup_provider_id(source_id) == requested).then_some(provider)
        })
    })
}

/// Effective overlay patches for a trouve-owned provider. A refreshed roster
/// replaces the bundled seed wholesale so models the account can no longer
/// use disappear instead of lingering from the seed.
fn owned_provider_models<'a>(
    state: &'a CatalogState,
    requested: &str,
) -> Option<&'a BTreeMap<String, Value>> {
    state
        .rosters
        .get(canonical_provider_id(requested))
        .filter(|roster| !roster.models.is_empty())
        .map(|roster| &roster.models)
        .or_else(|| overlay_provider_by_setup_id(&state.owned, requested).map(|p| &p.models))
}

/// Resolve one trouve-owned model against the newest available public base.
/// Objects deep-merge and arrays/scalars replace, matching models.dev's
/// `base_model` authoring semantics. The target map key remains the model id.
fn resolve_overlay_model(
    state: &CatalogState,
    target_id: &str,
    patch: &Value,
) -> Option<CatalogModel> {
    let mut patch = patch.clone();
    let patch_object = patch.as_object_mut()?;
    let base = patch_object
        .remove("base_model")
        .and_then(|value| value.as_str().map(String::from));
    let mut merged = match base {
        Some(base) => {
            let (provider, model) = base.split_once('/')?;
            serde_json::to_value(source_model(state, provider, model)?).ok()?
        }
        None => Value::Object(Map::new()),
    };
    merge_json(&mut merged, &patch);
    merged
        .as_object_mut()?
        .insert("id".into(), Value::String(target_id.into()));
    serde_json::from_value(merged).ok()
}

fn merge_json(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                if let Some(existing) = target.get_mut(key) {
                    merge_json(existing, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, patch) => *target = patch.clone(),
    }
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

fn parse_catalog_overlay(text: &str) -> Result<CatalogOverlay> {
    let catalog: CatalogOverlay =
        serde_json::from_str(text).context("parsing trouve model catalog")?;
    if catalog.is_empty() || catalog.values().all(|provider| provider.models.is_empty()) {
        bail!("trouve model catalog contains no models");
    }
    Ok(catalog)
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

/// Load every `<provider>.json` roster under `dir`. Missing, unparsable, or
/// stale-format files are skipped so the bundled seed keeps serving.
fn load_rosters(dir: &Path) -> BTreeMap<String, RosterState> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            let provider = path.file_stem()?.to_str()?.to_string();
            let roster = load_roster_file(&path).ok()??;
            Some((
                provider,
                RosterState {
                    models: roster.models,
                    fetched_at: Some(roster.fetched_at),
                    last_attempt: None,
                },
            ))
        })
        .collect()
}

fn load_roster_file(path: &Path) -> Result<Option<RosterFile>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let roster: RosterFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if roster.version != ROSTER_VERSION || roster.models.is_empty() {
        return Ok(None);
    }
    Ok(Some(roster))
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
        let catalog = ModelsDevCatalog::fixture();
        let gpt = catalog
            .model("openai", "openai", "gpt-5.6", OptionsDialect::OpenAi)
            .unwrap();
        assert_eq!(gpt.context_window, 1_050_000);
        assert_eq!(gpt.input_price_per_mtok, Some(4.0));
        assert!(gpt.supports_images);
        assert_eq!(
            gpt.options_schema
                .pointer("/properties/reasoning_effort/enum")
                .unwrap(),
            &json!(["none", "low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(
            gpt.options_schema
                .pointer("/properties/reasoning_effort/default"),
            Some(&json!("medium"))
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
    fn trouve_owned_codex_seed_overrides_only_what_codex_serves_differently() {
        let catalog = ModelsDevCatalog::fixture();
        let models = catalog.provider_models("openai-codex", "codex", OptionsDialect::CodexCli);
        assert_eq!(models.len(), 5, "seed = models with Codex-specific limits");

        // Codex serves Sol at a 500k window although the API record says 1.05M;
        // everything else (name, pricing, reasoning levels) is inherited until
        // the roster refresh brings the live values.
        let sol = models
            .iter()
            .find(|model| model.id == "codex/gpt-5.6-sol")
            .unwrap();
        assert_eq!(sol.display_name, "GPT-5.6 Sol");
        assert_eq!(sol.context_window, 500_000);
        assert_eq!(sol.input_price_per_mtok, Some(4.0));
        assert_eq!(
            sol.options_schema
                .pointer("/properties/reasoning_effort/enum"),
            Some(&json!(["none", "low", "medium", "high", "xhigh", "max"]))
        );
        assert!(
            sol.options_schema.pointer("/properties/fast").is_none(),
            "the fast tier is discovered live, not hand-written"
        );

        let astra = models
            .iter()
            .find(|model| model.id == "codex/gpt-6-astra")
            .unwrap();
        assert_eq!(astra.display_name, "GPT-6 Astra");
        assert_eq!(astra.context_window, 1_050_000);
        assert_eq!(
            catalog
                .model_record("openai-codex", "gpt-5.5")
                .unwrap()
                .limit
                .input,
            Some(272_000)
        );

        // The direct API surface remains the upstream models.dev record.
        assert_eq!(
            catalog
                .model("openai", "openai", "gpt-5.6-sol", OptionsDialect::OpenAi)
                .unwrap()
                .context_window,
            1_050_000
        );
        assert!(
            catalog
                .provider_presets()
                .iter()
                .all(|provider| provider.id != "openai-codex")
        );
    }

    #[test]
    fn trouve_owned_cursor_seed_covers_cursor_only_models_and_slug_remaps() {
        let catalog = ModelsDevCatalog::fixture();
        let models = catalog.provider_models("cursor", "cursor", OptionsDialect::ClaudeCli);
        let mut ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            [
                "cursor/composer-2.5",
                "cursor/default",
                "cursor/gemini-3.1-pro",
                "cursor/gemini-3.7-flash"
            ]
        );

        // A Cursor slug that differs from the models.dev id is remapped.
        let gemini = models
            .iter()
            .find(|model| model.id == "cursor/gemini-3.1-pro")
            .unwrap();
        assert_eq!(gemini.display_name, "Gemini 3.1 Pro");
        assert!(gemini.context_window > 0, "inherits the preview record");

        // Cursor-only models carry their own metadata.
        let composer = models
            .iter()
            .find(|model| model.id == "cursor/composer-2.5")
            .unwrap();
        assert_eq!(composer.context_window, 200_000);
        assert!(composer.supports_tools);
        assert_eq!(
            composer.options_schema.pointer("/properties/fast/default"),
            Some(&json!(false))
        );
    }

    #[test]
    fn shared_identity_follows_reviewed_base_models_only() {
        let catalog = ModelsDevCatalog::embedded();

        assert_eq!(
            catalog.shared_model_identity("openai", "gpt-5.6-sol"),
            Some("gpt-5.6-sol".into())
        );
        assert_eq!(
            catalog.shared_model_identity("openai-codex", "gpt-5.6-sol"),
            Some("gpt-5.6-sol".into())
        );
        assert_eq!(
            catalog.shared_model_identity("cursor", "gpt-5.6-sol"),
            Some("gpt-5.6-sol".into())
        );
        assert_eq!(
            catalog.shared_model_identity("cursor", "gemini-3.1-pro"),
            Some("gemini-3.1-pro-preview".into())
        );

        // Cursor-owned choices can still be selected explicitly but must not
        // acquire an automatic route merely because another provider later
        // publishes a coincidentally identical id.
        assert_eq!(catalog.shared_model_identity("cursor", "default"), None);
        assert_eq!(
            catalog.shared_model_identity("cursor", "composer-2.5"),
            None
        );

        // An owned choice becomes routable once the reviewed overlay gives it
        // an explicit public base-model identity.
        assert_eq!(
            catalog.shared_model_identity("cursor", "claude-opus-5"),
            Some("claude-opus-5".into())
        );
    }

    #[test]
    fn live_ids_are_only_an_availability_overlay() {
        let catalog = ModelsDevCatalog::fixture();
        let models = catalog.provider_models_for_ids(
            "openai",
            "codex",
            [
                "gpt-5.6",
                "text-embedding-4-large",
                "vendor-only",
                "gpt-5.6",
            ],
            OptionsDialect::CodexCli,
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "codex/gpt-5.6");
        assert_eq!(models[0].context_window, 1_050_000);
        assert!(
            models[0]
                .options_schema
                .pointer("/properties/reasoning_effort")
                .is_some()
        );
        assert!(
            models[0]
                .options_schema
                .pointer("/properties/temperature")
                .is_none()
        );
    }

    #[test]
    fn fixed_thinking_is_a_numeric_catalog_bound_not_invented_levels() {
        let catalog = ModelsDevCatalog::fixture();
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
        let catalog = ModelsDevCatalog::fixture();
        let model = catalog
            .model("openai", "openai", "gpt-5.6", OptionsDialect::OpenAi)
            .unwrap();
        let cost = catalog.cost_usd(&model, 200_000, 100_000, 10_000).unwrap();
        // Long-context tier: $8/M ordinary input, $0.80/M cached, $30/M output.
        assert!((cost - 1.98).abs() < 1e-10, "cost was {cost}");
    }

    #[test]
    fn snapshot_drives_provider_setup_catalog() {
        let catalog = ModelsDevCatalog::fixture();
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
        let catalog = ModelsDevCatalog::fixture();
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
        // Native AI SDK packages with no documented OpenAI-compatible or
        // Anthropic-compatible HTTP surface; nothing to adapt yet.
        assert_eq!(
            unsupported,
            ["gitlab", "qvac", "salad-cloud", "sap-ai-core", "watsonx"]
        );
    }

    #[test]
    fn valid_disk_cache_is_the_public_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models-dev-cache.json");
        let remote = parse_catalog(
            r#"{"openai":{"models":{"future":{"id":"future","name":"Future","tool_call":true,"limit":{"context":42}},"gpt-5.6-sol":{"id":"gpt-5.6-sol","name":"Remote Sol","tool_call":true,"reasoning_options":[{"type":"effort","values":["low","medium"]}],"limit":{"context":777000,"input":649000,"output":128000},"cost":{"input":9.0,"output":18.0}}}}}"#,
        )
        .unwrap();
        let cache = DiskCache {
            version: CACHE_VERSION,
            fetched_at: unix_now(),
            etag: Some("test".into()),
            catalog: remote,
        };
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
        let catalog = ModelsDevCatalog::from_paths(Some(path), None);
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
                .is_none(),
            "nothing is bundled: only the downloaded catalog is served"
        );
        assert!(catalog.is_available());

        // Owned records resolve their base lazily, so a refreshed public
        // catalog updates inherited fields without replacing Codex-specific
        // limits, defaults, or reasoning levels.
        let codex = catalog
            .model(
                "openai-codex",
                "codex",
                "gpt-5.6-sol",
                OptionsDialect::CodexCli,
            )
            .unwrap();
        assert_eq!(codex.display_name, "Remote Sol");
        assert_eq!(codex.context_window, 500_000);
        assert_eq!(codex.input_price_per_mtok, Some(9.0));
        assert_eq!(
            codex
                .options_schema
                .pointer("/properties/reasoning_effort/enum"),
            Some(&json!(["low", "medium"])),
            "seed overrides keep inheriting reasoning levels from the download"
        );
    }

    fn codex_ids(catalog: &ModelsDevCatalog) -> Vec<String> {
        let mut ids: Vec<_> = catalog
            .provider_models("openai-codex", "codex", OptionsDialect::CodexCli)
            .into_iter()
            .map(|model| model.id)
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn roster_file_replaces_the_bundled_overlay_provider() {
        let dir = tempfile::tempdir().unwrap();
        let rosters = dir.path().join("rosters");
        std::fs::create_dir_all(&rosters).unwrap();
        let roster = json!({
            "version": ROSTER_VERSION,
            "fetched_at": unix_now(),
            "models": {
                "gpt-5.6-luna": {
                    "base_model": "openai/gpt-5.6-luna",
                    "reasoning_options": [{"type": "effort", "values": ["low", "high"], "default": "high"}]
                },
                "gpt-7-nova": {"name": "GPT-7 Nova", "tool_call": true, "attachment": true}
            }
        });
        std::fs::write(
            rosters.join("openai-codex.json"),
            serde_json::to_vec(&roster).unwrap(),
        )
        .unwrap();

        let catalog = ModelsDevCatalog::fixture_for_data_dir(dir.path());
        assert_eq!(
            codex_ids(&catalog),
            ["codex/gpt-5.6-luna", "codex/gpt-7-nova"]
        );
        assert!(!catalog.roster_needs_refresh("openai-codex"));
        assert_eq!(catalog.owned_provider_models("openai-codex").len(), 2);

        let luna = catalog
            .model(
                "openai-codex",
                "codex",
                "gpt-5.6-luna",
                OptionsDialect::CodexCli,
            )
            .unwrap();
        assert!(
            luna.context_window > 0,
            "inherits limits from the public base"
        );
        assert_eq!(
            luna.options_schema
                .pointer("/properties/reasoning_effort/default"),
            Some(&json!("high"))
        );
        let nova = catalog
            .model(
                "openai-codex",
                "codex",
                "gpt-7-nova",
                OptionsDialect::CodexCli,
            )
            .unwrap();
        assert_eq!(nova.display_name, "GPT-7 Nova");
        assert_eq!(nova.context_window, 0);
        assert!(nova.supports_images);

        // Other overlay providers are untouched.
        assert!(
            !catalog
                .provider_models("cursor", "cursor", OptionsDialect::OpenAi)
                .is_empty()
        );
    }

    #[test]
    fn invalid_roster_files_fall_back_to_the_bundled_seed() {
        let dir = tempfile::tempdir().unwrap();
        let rosters = dir.path().join("rosters");
        std::fs::create_dir_all(&rosters).unwrap();
        std::fs::write(rosters.join("openai-codex.json"), b"{not json").unwrap();
        let catalog = ModelsDevCatalog::fixture_for_data_dir(dir.path());
        assert_eq!(codex_ids(&catalog), codex_ids(&ModelsDevCatalog::fixture()));
        assert!(catalog.roster_needs_refresh("openai-codex"));

        // A stale format version is ignored too.
        std::fs::write(
            rosters.join("openai-codex.json"),
            serde_json::to_vec(&json!({
                "version": ROSTER_VERSION + 1,
                "fetched_at": unix_now(),
                "models": {"gpt-5.6-luna": {"base_model": "openai/gpt-5.6-luna"}}
            }))
            .unwrap(),
        )
        .unwrap();
        let catalog = ModelsDevCatalog::fixture_for_data_dir(dir.path());
        assert_eq!(codex_ids(&catalog), codex_ids(&ModelsDevCatalog::fixture()));
    }

    #[tokio::test]
    async fn replace_roster_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = ModelsDevCatalog::fixture_for_data_dir(dir.path());
        assert!(catalog.roster_needs_refresh("openai-codex"));
        assert!(catalog.begin_roster_refresh("openai-codex"));
        assert!(
            !catalog.begin_roster_refresh("openai-codex"),
            "overlapping triggers admit one refresh per retry window"
        );
        assert!(
            !catalog.roster_needs_refresh("openai-codex"),
            "retry backoff suppresses immediate re-attempts"
        );

        assert!(
            catalog
                .replace_roster("openai-codex", BTreeMap::new())
                .await
                .is_err(),
            "an empty roster must never replace the seed"
        );

        let mut models = BTreeMap::new();
        models.insert(
            "gpt-5.6-luna".to_string(),
            json!({"base_model": "openai/gpt-5.6-luna"}),
        );
        catalog
            .replace_roster("openai-codex", models)
            .await
            .unwrap();
        assert_eq!(codex_ids(&catalog), ["codex/gpt-5.6-luna"]);
        assert!(!catalog.roster_needs_refresh("openai-codex"));

        let path = dir.path().join("rosters").join("openai-codex.json");
        let file: RosterFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file.version, ROSTER_VERSION);
        assert_eq!(file.models.len(), 1);

        let reloaded = ModelsDevCatalog::fixture_for_data_dir(dir.path());
        assert_eq!(codex_ids(&reloaded), ["codex/gpt-5.6-luna"]);
        assert!(!reloaded.roster_needs_refresh("openai-codex"));
    }

    #[tokio::test]
    async fn concurrent_roster_writes_never_corrupt_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = std::sync::Arc::new(ModelsDevCatalog::fixture_for_data_dir(dir.path()));
        let writes = (0..8).map(|index| {
            let catalog = catalog.clone();
            tokio::spawn(async move {
                let mut models = BTreeMap::new();
                models.insert(
                    format!("model-{index}"),
                    json!({"name": format!("Model {index}"), "tool_call": true}),
                );
                catalog.replace_roster("openai-codex", models).await
            })
        });
        for write in writes {
            write.await.unwrap().unwrap();
        }
        // Whichever write won, the file is complete and matches a full roster.
        let path = dir.path().join("rosters").join("openai-codex.json");
        let file: RosterFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file.models.len(), 1);
        assert!(
            std::fs::read_dir(dir.path().join("rosters"))
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == "openai-codex.json"),
            "no temp files are left behind"
        );
    }

    #[test]
    fn empty_catalog_serves_only_overlay_only_models() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = ModelsDevCatalog::for_data_dir(dir.path());
        assert!(!catalog.is_available());
        assert!(catalog.provider_presets().is_empty());
        assert!(
            catalog
                .provider_for_endpoint("openai", "https://api.openai.com/v1", "openai-compat")
                .is_none()
        );
        assert!(
            catalog
                .model("openai", "openai", "gpt-5.6", OptionsDialect::OpenAi)
                .is_none()
        );
        // Every Codex seed entry inherits from a public record, so nothing
        // can be described yet.
        assert!(codex_ids(&catalog).is_empty());
        // Cursor-only seed entries carry their own metadata.
        let mut cursor: Vec<_> = catalog
            .provider_models("cursor", "cursor", OptionsDialect::ClaudeCli)
            .into_iter()
            .map(|model| model.id)
            .collect();
        cursor.sort();
        assert_eq!(
            cursor,
            [
                "cursor/composer-2.5",
                "cursor/default",
                "cursor/gemini-3.7-flash"
            ]
        );
        assert!(!catalog.has_source_model("openai", "gpt-5.6"));
    }
}
