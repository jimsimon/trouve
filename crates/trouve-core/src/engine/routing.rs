//! Provider-neutral turn coordination.
//!
//! Native chat providers and vendor-agent backends keep their own execution
//! mechanics, but report one common attempt outcome here. The persisted
//! transcript and session worktree are the handoff boundary between them.

use super::*;

/// Bound automatic failover so one turn cannot churn through every configured
/// credential. Persisted circuit state advances later turns past failed routes.
const MAX_ROUTE_ATTEMPTS_PER_TURN: usize = 4;
#[cfg(not(test))]
const MODEL_ROUTE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const MODEL_ROUTE_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(super) struct ModelCandidate {
    provider_id: String,
    provider_model: String,
    provider_generation: u64,
    info: trouve_protocol::ModelInfo,
    executor: ModelExecutor,
    shared_model_id: Option<String>,
}

#[derive(Clone)]
pub(super) struct RoutedAttemptSnapshot {
    pub(super) provider_id: String,
    pub(super) provider_model: String,
    pub(super) provider_generation: u64,
    pub(super) attempt_order: i64,
}

#[derive(Clone)]
enum ModelExecutor {
    Native(Arc<dyn Provider>),
    Backend(Arc<dyn AgentBackend>),
}

type ProviderRegistryEntry = (String, u64, Arc<dyn Provider>);
type BackendRegistryEntry = (String, u64, Arc<dyn AgentBackend>);
type ProviderRegistrySnapshot = (Vec<ProviderRegistryEntry>, Vec<BackendRegistryEntry>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteFailureKind {
    Capacity,
    Authentication,
    Unavailable,
}

impl RouteFailureKind {
    pub(super) fn cooldown(self) -> (i64, i64) {
        match self {
            Self::Capacity => (5 * 60, 6 * 60 * 60),
            Self::Authentication => (60 * 60, 24 * 60 * 60),
            Self::Unavailable => (30, 30 * 60),
        }
    }

    fn failover_reason(self) -> trouve_protocol::ModelRouteReason {
        match self {
            Self::Capacity => trouve_protocol::ModelRouteReason::CapacityFailover,
            Self::Authentication | Self::Unavailable => {
                trouve_protocol::ModelRouteReason::RouteFailover
            }
        }
    }
}

#[derive(Debug)]
struct RouteAttemptFailure {
    kind: RouteFailureKind,
    message: String,
    safe_to_retry: bool,
}

enum RouteAttemptResult {
    Completed,
    Cancelled,
    Failed(RouteAttemptFailure),
}

struct TurnAccounting {
    model: String,
    usage: Usage,
    context_input_tokens: u64,
    cost_known: bool,
}

impl Default for TurnAccounting {
    fn default() -> Self {
        Self {
            model: String::new(),
            usage: Usage::default(),
            context_input_tokens: 0,
            cost_known: true,
        }
    }
}

impl TurnAccounting {
    fn add_native(
        &mut self,
        catalog: &trouve_providers::models_dev::ModelsDevCatalog,
        route: &ModelCandidate,
        usage: &Usage,
    ) {
        self.usage.input_tokens += usage.input_tokens;
        self.usage.output_tokens += usage.output_tokens;
        self.usage.cached_input_tokens += usage.cached_input_tokens;
        match catalog.cost_usd(
            &route.info,
            usage.input_tokens,
            usage.cached_input_tokens,
            usage.output_tokens,
        ) {
            Some(cost) => self.usage.cost_usd = Some(self.usage.cost_usd.unwrap_or(0.0) + cost),
            None => self.cost_known = false,
        }
        if usage.context_window.is_some() {
            self.usage.context_window = usage.context_window;
        }
        self.context_input_tokens = usage
            .context_input_tokens
            .unwrap_or_else(|| usage.input_tokens.saturating_add(usage.cached_input_tokens));
        self.usage.context_input_tokens = Some(self.context_input_tokens);
    }

    fn add_backend(&mut self, usage: &Usage) {
        self.usage.input_tokens += usage.input_tokens;
        self.usage.output_tokens += usage.output_tokens;
        self.usage.cached_input_tokens += usage.cached_input_tokens;
        match usage.cost_usd {
            Some(cost) => self.usage.cost_usd = Some(self.usage.cost_usd.unwrap_or(0.0) + cost),
            None => self.cost_known = false,
        }
        if usage.context_window.is_some() {
            self.usage.context_window = usage.context_window;
        }
        self.context_input_tokens = usage
            .context_input_tokens
            .unwrap_or_else(|| usage.input_tokens.saturating_add(usage.cached_input_tokens));
        self.usage.context_input_tokens = Some(self.context_input_tokens);
    }

    fn finalize_cost(&mut self) {
        if !self.cost_known {
            self.usage.cost_usd = None;
        }
    }
}

impl ModelCandidate {
    fn automatic_selection_id(&self) -> Option<String> {
        self.shared_model_id
            .as_deref()
            .and_then(neutral_model_id)
            .map(|model| format!("auto/{model}"))
    }

    fn concrete_selection_id(&self) -> String {
        format!("{}/{}", self.provider_id, self.provider_model)
    }
}

/// Automatic ids are namespaced in the current protocol. Bare names remain
/// accepted for clients that stored a selection before that namespace existed.
pub(super) fn automatic_model_name(selection: &str) -> Option<&str> {
    selection
        .strip_prefix("auto/")
        .filter(|model| neutral_model_id(model).is_some())
        .or_else(|| {
            (!selection.contains('/') && neutral_model_id(selection).is_some()).then_some(selection)
        })
}

pub(super) fn valid_concrete_selection(selection: &str) -> Option<(&str, &str)> {
    let (provider, model) = selection.split_once('/')?;
    (provider == provider.trim()
        && model == model.trim()
        && !provider.is_empty()
        && !model.is_empty()
        && model
            .split('/')
            .all(|segment| !segment.is_empty() && segment == segment.trim()))
    .then_some((provider, model))
}

fn neutral_model_id(provider_model: &str) -> Option<&str> {
    let id = provider_model.trim();
    if id.is_empty()
        || id != provider_model
        || id
            .split('/')
            .any(|segment| segment.is_empty() || segment != segment.trim())
    {
        return None;
    }
    if matches!(
        id.to_ascii_lowercase().as_str(),
        "auto" | "automatic" | "default" | "latest"
    ) {
        return None;
    }
    Some(id)
}

fn model_name_for_provider<'a>(provider_id: &str, qualified_id: &'a str) -> &'a str {
    qualified_id
        .strip_prefix(provider_id)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(qualified_id)
}

fn adapter_can_route_automatic_model(
    provider_id: &str,
    automatic_model: &str,
    static_models: &[trouve_protocol::ModelInfo],
    mut shared_identity: impl FnMut(&str) -> Option<String>,
) -> bool {
    // Some open-ended adapters can identify a live-only model without
    // advertising it statically. Catalog-backed adapters may instead expose a
    // provider-local alias whose reviewed base model is the automatic id.
    shared_identity(automatic_model).as_deref() == Some(automatic_model)
        || static_models.iter().any(|info| {
            let provider_model = model_name_for_provider(provider_id, &info.id);
            shared_identity(provider_model).as_deref() == Some(automatic_model)
        })
}

fn fallback_model_info(qualified_id: &str, provider_model: &str) -> trouve_protocol::ModelInfo {
    trouve_protocol::ModelInfo {
        id: qualified_id.to_string(),
        display_name: provider_model.to_string(),
        context_window: 0,
        supports_tools: true,
        supports_images: false,
        input_price_per_mtok: None,
        output_price_per_mtok: None,
        options_schema: serde_json::json!({"type": "object", "properties": {}}),
    }
}

fn thinking_schema(model: &trouve_protocol::ModelInfo) -> Option<(Vec<String>, Option<String>)> {
    thinking_option_property(model).map(|(_, property, values)| {
        (
            values
                .iter()
                .filter_map(|value| value.as_str().map(String::from))
                .collect(),
            property["default"].as_str().map(String::from),
        )
    })
}

fn routed_options_schema(models: &[&trouve_protocol::ModelInfo]) -> serde_json::Value {
    let mut properties = models
        .first()
        .and_then(|model| model.options_schema["properties"].as_object())
        .cloned()
        .unwrap_or_default();
    properties.retain(|key, value| {
        !THINKING_OPTION_KEYS.contains(&key.as_str())
            && models.iter().skip(1).all(|model| {
                model.options_schema["properties"]
                    .get(key)
                    .is_some_and(|candidate| candidate == value)
            })
    });

    let mut schemas = models.iter().map(|model| thinking_schema(model));
    if let Some(Some((mut common_levels, first_default))) = schemas.next() {
        let mut shared_default = first_default;
        let shared_by_every_route = schemas.all(|schema| {
            let Some((values, default)) = schema else {
                return false;
            };
            common_levels.retain(|value| values.contains(value));
            if default != shared_default {
                shared_default = None;
            }
            true
        });
        if shared_by_every_route && common_levels.len() > 1 {
            let default = shared_default
                .filter(|value| common_levels.contains(value))
                .or_else(|| {
                    common_levels
                        .iter()
                        .find(|value| value.as_str() == "medium")
                        .cloned()
                })
                .unwrap_or_else(|| common_levels[0].clone());
            properties.insert(
                "thinking_level".into(),
                serde_json::json!({
                    "type": "string",
                    "enum": common_levels,
                    "default": default,
                    "description": "How much thinking the model does before answering"
                }),
            );
        }
    }
    serde_json::json!({"type": "object", "properties": properties})
}

/// Project thread-level options onto one concrete route. Automatic model
/// schemas expose only portable/common controls; normalize the canonical
/// thinking choice to the route's provider-specific key before filtering.
fn model_options_for_schema(
    options: &serde_json::Map<String, serde_json::Value>,
    model: &trouve_protocol::ModelInfo,
) -> serde_json::Map<String, serde_json::Value> {
    let mut filtered = options.clone();
    normalize_thinking_option(&mut filtered, Some(model));
    let properties = model.options_schema["properties"].as_object();
    filtered.retain(|key, _| properties.is_some_and(|properties| properties.contains_key(key)));
    filtered
}

fn common_price(
    candidates: &[ModelCandidate],
    get: impl Fn(&trouve_protocol::ModelInfo) -> Option<f64>,
) -> Option<f64> {
    let first = get(&candidates.first()?.info)?;
    candidates
        .iter()
        .all(|candidate| get(&candidate.info) == Some(first))
        .then_some(first)
}

fn routed_model_info(
    id: String,
    mut candidates: Vec<ModelCandidate>,
) -> trouve_protocol::RoutedModelInfo {
    candidates.sort_by(|a, b| {
        a.provider_id
            .cmp(&b.provider_id)
            .then_with(|| a.provider_model.cmp(&b.provider_model))
    });
    let first = &candidates[0].info;
    let display_name = if candidates
        .iter()
        .all(|candidate| candidate.info.display_name == first.display_name)
    {
        first.display_name.clone()
    } else {
        id.clone()
    };
    let context_window = candidates
        .iter()
        .map(|candidate| candidate.info.context_window)
        .filter(|window| *window > 0)
        .min()
        .unwrap_or(0);
    let automatic = id.starts_with("auto/");
    trouve_protocol::RoutedModelInfo {
        id,
        display_name,
        context_window,
        supports_tools: candidates
            .iter()
            .all(|candidate| candidate.info.supports_tools),
        supports_images: candidates
            .iter()
            .all(|candidate| candidate.info.supports_images),
        input_price_per_mtok: common_price(&candidates, |model| model.input_price_per_mtok),
        output_price_per_mtok: common_price(&candidates, |model| model.output_price_per_mtok),
        options_schema: if candidates.len() == 1 && !automatic {
            first.options_schema.clone()
        } else {
            routed_options_schema(
                &candidates
                    .iter()
                    .map(|candidate| &candidate.info)
                    .collect::<Vec<_>>(),
            )
        },
        routes: candidates
            .iter()
            .map(|candidate| trouve_protocol::ModelRouteInfo {
                provider_id: candidate.provider_id.clone(),
                provider_model: candidate.provider_model.clone(),
            })
            .collect(),
    }
}

fn model_info_for_routed_selection(
    routed: trouve_protocol::RoutedModelInfo,
) -> trouve_protocol::ModelInfo {
    trouve_protocol::ModelInfo {
        id: routed.id,
        display_name: routed.display_name,
        context_window: routed.context_window,
        supports_tools: routed.supports_tools,
        supports_images: routed.supports_images,
        input_price_per_mtok: routed.input_price_per_mtok,
        output_price_per_mtok: routed.output_price_per_mtok,
        options_schema: routed.options_schema,
    }
}

fn routed_model_catalog(candidates: Vec<ModelCandidate>) -> Vec<trouve_protocol::RoutedModelInfo> {
    let mut grouped = std::collections::BTreeMap::<String, Vec<ModelCandidate>>::new();
    for candidate in candidates {
        grouped
            .entry(candidate.concrete_selection_id())
            .or_default()
            .push(candidate.clone());
        if let Some(id) = candidate.automatic_selection_id() {
            grouped.entry(id).or_default().push(candidate);
        }
    }
    grouped
        .into_iter()
        .map(|(id, candidates)| routed_model_info(id, candidates))
        .collect()
}

pub(super) fn compatibility_model_catalog(
    candidates: Vec<ModelCandidate>,
) -> Vec<trouve_protocol::ModelInfo> {
    routed_model_catalog(candidates)
        .into_iter()
        .map(model_info_for_routed_selection)
        .collect()
}

fn subscription_health_rank(health: &trouve_protocol::SubscriptionHealth) -> (u8, i64) {
    match health.status.as_str() {
        "ok" => match health
            .windows
            .iter()
            .map(|window| window.used_percent.max(0))
            .max()
        {
            Some(used) if used >= 100 => (3, used),
            Some(used) => (0, used),
            None => (1, 0),
        },
        "unavailable" => (2, 0),
        _ => (1, 0),
    }
}

fn native_attempt_failure(
    error: trouve_providers::ProviderError,
    side_effect_started: bool,
) -> RouteAttemptFailure {
    let kind = if error.is_capacity_exhausted() {
        RouteFailureKind::Capacity
    } else if matches!(&error, trouve_providers::ProviderError::Auth(_)) {
        RouteFailureKind::Authentication
    } else {
        RouteFailureKind::Unavailable
    };
    RouteAttemptFailure {
        kind,
        message: format!("provider error: {error}"),
        safe_to_retry: !side_effect_started,
    }
}

fn backend_attempt_failure(error: BackendError, side_effect_started: bool) -> RouteAttemptFailure {
    let capacity = error.is_capacity_exhausted();
    let kind = if capacity {
        RouteFailureKind::Capacity
    } else if matches!(
        &error,
        BackendError::Auth(_) | BackendError::NotInstalled(_)
    ) {
        RouteFailureKind::Authentication
    } else {
        RouteFailureKind::Unavailable
    };
    RouteAttemptFailure {
        kind,
        message: format!("backend error: {error}"),
        safe_to_retry: !side_effect_started,
    }
}

fn backend_permission_policy(tools_enabled: bool, persona_read_only: bool) -> BackendPermission {
    if !tools_enabled || persona_read_only {
        BackendPermission::ReadOnly
    } else {
        BackendPermission::Ask
    }
}

fn backend_strict_tool_free_policy(tools_enabled: bool, supports_tool_free_turns: bool) -> bool {
    !tools_enabled && supports_tool_free_turns
}

/// Real adapters bound their own cancellation acknowledgement. This outer
/// deadline also protects the dispatcher from injected/custom backends that
/// violate `BackendTurn`'s cleanup contract.
#[cfg(not(test))]
const BACKEND_CANCEL_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const BACKEND_CANCEL_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);

/// A fair writer queued on the admission gate before backend cancellation.
///
/// The acquisition future is intentionally retained after its first poll.
/// Pending writers stay in Tokio's fair queue, while an immediately acquired
/// writer guard closes the gate directly. Either form prevents a mutation
/// already queued on the execution lane from starting during backend cleanup.
struct BackendMutationQuarantine {
    _pending_acquisition:
        Option<futures::future::BoxFuture<'static, tokio::sync::OwnedRwLockWriteGuard<()>>>,
    _guard: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

impl BackendMutationQuarantine {
    async fn queue(mutation_admission: Arc<tokio::sync::RwLock<()>>) -> Self {
        let mut acquisition = mutation_admission.write_owned().boxed();
        let guard = futures::future::poll_fn(|context| {
            std::task::Poll::Ready(
                match std::future::Future::poll(acquisition.as_mut(), context) {
                    std::task::Poll::Ready(guard) => Some(guard),
                    std::task::Poll::Pending => None,
                },
            )
        })
        .await;
        Self {
            _pending_acquisition: guard.is_none().then_some(acquisition),
            _guard: guard,
        }
    }
}

/// Drain a cancelled backend through its cleanup acknowledgement. A backend
/// that misses the deadline is detached from the turn's scheduler resources,
/// but the session mutation lane stays fenced until its stream really closes.
async fn drain_or_quarantine_backend(
    mut stream: trouve_agents::BackendEventStream,
    attempt_cancel: tokio_util::sync::CancellationToken,
    mutation_admission: Arc<tokio::sync::RwLock<()>>,
    mutation_permits: Vec<SessionMutationPermit>,
    backend_id: String,
    thread_id: String,
) -> bool {
    // An in-flight vendor-native mutation already owns both the execution
    // lane and an admission reader. Otherwise queue the exclusive admission
    // fence before cancellation; fair ordering then blocks mutations that
    // were waiting for execution before this failure was observed.
    let mutation_quarantine = if mutation_permits.is_empty() {
        Some(BackendMutationQuarantine::queue(mutation_admission).await)
    } else {
        None
    };
    attempt_cancel.cancel();
    if tokio::time::timeout(BACKEND_CANCEL_CLEANUP_TIMEOUT, async {
        while stream.next().await.is_some() {}
    })
    .await
    .is_ok()
    {
        return false;
    }

    tracing::warn!(
        backend_id,
        thread_id,
        timeout_ms = BACKEND_CANCEL_CLEANUP_TIMEOUT.as_millis(),
        "backend cancellation cleanup timed out; quarantining the session mutation lane"
    );
    tokio::spawn(async move {
        let _mutation_quarantine = mutation_quarantine;
        let _mutation_permits = mutation_permits;
        while stream.next().await.is_some() {}
        tracing::warn!(
            backend_id,
            thread_id,
            "late backend cancellation cleanup completed; session lane released"
        );
    });
    true
}

pub(super) fn unfinished_collaborator_reason(
    cancelled: bool,
    attempt_error: Option<&anyhow::Error>,
    backend_error: Option<&BackendError>,
) -> String {
    if cancelled {
        "turn cancelled".to_string()
    } else if let Some(error) = attempt_error {
        format!("parent turn event processing failed: {error}")
    } else if let Some(error) = backend_error {
        format!("parent backend stream failed: {error}")
    } else {
        "parent turn ended before collaborator completion".to_string()
    }
}

impl Engine {
    /// Model-selector catalog for current clients. Shared hosted models have
    /// an `auto/<model>` entry plus one concrete entry per provider. Local,
    /// loopback, and transport-owned models remain concrete-only.
    pub async fn list_model_routes(&self) -> Vec<trouve_protocol::RoutedModelInfo> {
        routed_model_catalog(self.refresh_model_candidates().await)
    }

    pub(super) fn available_model_candidates(&self) -> Vec<ModelCandidate> {
        let online = self.is_online();
        let offline_capable = if online {
            HashSet::new()
        } else {
            self.offline_capable_provider_ids()
        };
        let (providers, backends) = self.provider_registry_snapshot();
        let mut candidates = Vec::new();
        for (provider_id, provider_generation, provider) in providers
            .into_iter()
            .filter(|(id, _, _)| online || offline_capable.contains(id))
        {
            candidates.extend(provider.models().into_iter().map(|info| {
                let provider_model = model_name_for_provider(&provider_id, &info.id).to_string();
                ModelCandidate {
                    shared_model_id: provider.shared_model_identity(&provider_model),
                    provider_model,
                    provider_id: provider_id.clone(),
                    provider_generation,
                    info,
                    executor: ModelExecutor::Native(provider.clone()),
                }
            }));
        }
        if online {
            for (provider_id, provider_generation, backend) in backends {
                let status = backend.status();
                if !status.installed || !status.has_credentials {
                    continue;
                }
                candidates.extend(backend.models().into_iter().map(|info| {
                    let provider_model =
                        model_name_for_provider(&provider_id, &info.id).to_string();
                    ModelCandidate {
                        shared_model_id: backend.shared_model_identity(&provider_model),
                        provider_model,
                        provider_id: provider_id.clone(),
                        provider_generation,
                        info,
                        executor: ModelExecutor::Backend(backend.clone()),
                    }
                }));
            }
        }
        candidates
    }

    pub(super) fn known_automatic_model_info(
        &self,
        selection: &str,
    ) -> Option<trouve_protocol::ModelInfo> {
        let automatic_model = automatic_model_name(selection)?;
        let candidates = self
            .available_model_candidates()
            .into_iter()
            .filter(|candidate| candidate.shared_model_id.as_deref() == Some(automatic_model))
            .collect::<Vec<_>>();
        (!candidates.is_empty()).then(|| {
            model_info_for_routed_selection(routed_model_info(selection.to_string(), candidates))
        })
    }

    pub(super) async fn refresh_model_candidates(&self) -> Vec<ModelCandidate> {
        self.refresh_model_candidates_for(None).await
    }

    /// Refresh only adapters which can satisfy `selection`. All eligible
    /// discoveries run concurrently and each has a hard deadline, so a large
    /// broken provider roster does not become a serial preflight chain.
    async fn refresh_model_candidates_for(&self, selection: Option<&str>) -> Vec<ModelCandidate> {
        let online = self.is_online();
        if online
            && self.connectivity_probe.is_some()
            && let Ok(Err(error)) = tokio::time::timeout(
                MODEL_ROUTE_DISCOVERY_TIMEOUT,
                self.model_catalog.refresh_if_stale(),
            )
            .await
        {
            tracing::debug!("models.dev refresh failed; using cached snapshot: {error:#}");
        }
        let automatic = selection.and_then(automatic_model_name);
        let concrete_provider = selection
            .filter(|_| automatic.is_none())
            .and_then(|selection| selection.split_once('/'))
            .map(|(provider, _)| provider);
        let offline_capable = if online {
            HashSet::new()
        } else {
            self.offline_capable_provider_ids()
        };
        let (providers, backends) = self.provider_registry_snapshot();
        let providers = providers
            .into_iter()
            .filter(|(id, _, _)| online || offline_capable.contains(id))
            .filter(|(id, _, provider)| {
                automatic.is_none_or(|model| {
                    adapter_can_route_automatic_model(id, model, &provider.models(), |candidate| {
                        provider.shared_model_identity(candidate)
                    })
                }) && concrete_provider.is_none_or(|selected| id == selected)
            })
            .collect::<Vec<_>>();
        let provider_lists = futures::future::join_all(providers.into_iter().map(
            |(provider_id, provider_generation, provider)| async move {
                let models = match tokio::time::timeout(
                    MODEL_ROUTE_DISCOVERY_TIMEOUT,
                    provider.list_models(),
                )
                .await
                {
                    Ok(models) => models,
                    Err(_) => {
                        tracing::warn!(
                            provider = %provider_id,
                            "model discovery timed out; using the static catalog"
                        );
                        provider.models()
                    }
                };
                (provider_id, provider_generation, provider, models)
            },
        ))
        .await;
        let mut candidates = Vec::new();
        for (provider_id, provider_generation, provider, models) in provider_lists {
            candidates.extend(models.into_iter().map(|info| {
                let provider_model = model_name_for_provider(&provider_id, &info.id).to_string();
                ModelCandidate {
                    shared_model_id: provider.shared_model_identity(&provider_model),
                    provider_model,
                    provider_id: provider_id.clone(),
                    provider_generation,
                    info,
                    executor: ModelExecutor::Native(provider.clone()),
                }
            }));
        }

        if online {
            let ready = backends
                .into_iter()
                .filter(|(_, _, backend)| {
                    let status = backend.status();
                    status.installed && status.has_credentials
                })
                .filter(|(id, _, backend)| {
                    automatic.is_none_or(|model| {
                        adapter_can_route_automatic_model(
                            id,
                            model,
                            &backend.models(),
                            |candidate| backend.shared_model_identity(candidate),
                        )
                    }) && concrete_provider.is_none_or(|selected| id == selected)
                })
                .collect::<Vec<_>>();
            let listings = futures::future::join_all(ready.into_iter().map(
                |(provider_id, provider_generation, backend)| async move {
                    let models = match tokio::time::timeout(
                        MODEL_ROUTE_DISCOVERY_TIMEOUT,
                        backend.list_models(),
                    )
                    .await
                    {
                        Ok(models) => models,
                        Err(_) => {
                            tracing::warn!(
                                provider = %provider_id,
                                "backend model discovery timed out; using the static catalog"
                            );
                            backend.models()
                        }
                    };
                    (provider_id, provider_generation, backend, models)
                },
            ))
            .await;
            for (provider_id, provider_generation, backend, models) in listings {
                candidates.extend(models.into_iter().map(|info| {
                    let provider_model =
                        model_name_for_provider(&provider_id, &info.id).to_string();
                    ModelCandidate {
                        shared_model_id: backend.shared_model_identity(&provider_model),
                        provider_model,
                        provider_id: provider_id.clone(),
                        provider_generation,
                        info,
                        executor: ModelExecutor::Backend(backend.clone()),
                    }
                }));
            }
        }
        candidates
    }

    /// Resolve an automatic id to runnable routes. Concrete ids remain hard
    /// pins and therefore always return at most one route.
    async fn resolve_model_candidates(
        &self,
        thread: &Thread,
    ) -> Result<Vec<ModelCandidate>, EngineError> {
        let model = thread.model.as_str();
        if automatic_model_name(model).is_some() {
            let affinity = self
                .store
                .thread_route_affinity(&thread.id)
                .map_err(EngineError::Internal)?;
            return self
                .resolve_automatic_model_candidates(model, affinity.as_ref())
                .await;
        }

        let all = self.refresh_model_candidates_for(Some(model)).await;

        if let Some(candidate) = all
            .iter()
            .find(|candidate| candidate.concrete_selection_id() == model)
        {
            return Ok(vec![candidate.clone()]);
        }
        // Preserve the explicit escape hatch for custom providers whose model
        // roster is intentionally open-ended.
        if let Some((provider_id, provider_model)) = valid_concrete_selection(model) {
            let (providers, backends) = self.provider_registry_snapshot();
            if let Some((_, provider_generation, provider)) =
                providers.into_iter().find(|(id, _, _)| id == provider_id)
            {
                return Ok(vec![ModelCandidate {
                    provider_id: provider_id.to_string(),
                    provider_model: provider_model.to_string(),
                    provider_generation,
                    info: fallback_model_info(model, provider_model),
                    executor: ModelExecutor::Native(provider),
                    shared_model_id: None,
                }]);
            }
            if let Some((_, provider_generation, backend)) =
                backends.into_iter().find(|(id, _, _)| id == provider_id)
            {
                return Ok(vec![ModelCandidate {
                    provider_id: provider_id.to_string(),
                    provider_model: provider_model.to_string(),
                    provider_generation,
                    info: fallback_model_info(model, provider_model),
                    executor: ModelExecutor::Backend(backend),
                    shared_model_id: None,
                }]);
            }
        }
        Err(EngineError::BadRequest(format!(
            "selected provider route {model} is not configured or available"
        )))
    }

    async fn resolve_automatic_model_candidates(
        &self,
        selection: &str,
        affinity: Option<&(String, String)>,
    ) -> Result<Vec<ModelCandidate>, EngineError> {
        let automatic_model = automatic_model_name(selection).ok_or_else(|| {
            EngineError::BadRequest(format!("invalid automatic model selection {selection}"))
        })?;
        let matching = |candidates: Vec<ModelCandidate>| {
            candidates
                .into_iter()
                .filter(|candidate| candidate.shared_model_id.as_deref() == Some(automatic_model))
                .collect::<Vec<_>>()
        };

        // A known static route is enough to start a turn. Live discovery is a
        // fallback for account-specific models, not a 30-provider preflight on
        // every prompt.
        let mut candidates = matching(self.available_model_candidates());
        if candidates.is_empty() {
            candidates = matching(self.refresh_model_candidates_for(Some(selection)).await);
        }
        if candidates.is_empty() {
            return Err(EngineError::BadRequest(format!(
                "no provider is configured and available for {selection}"
            )));
        }
        self.rank_model_candidates(selection, candidates, affinity)
            .await
    }

    async fn resolve_background_attach_candidate(
        &self,
        selection: &str,
        backend_id: &str,
    ) -> Result<Vec<ModelCandidate>, EngineError> {
        let automatic_model = automatic_model_name(selection).ok_or_else(|| {
            EngineError::BadRequest(format!("invalid automatic model selection {selection}"))
        })?;
        let matching = |candidates: Vec<ModelCandidate>| {
            candidates
                .into_iter()
                .filter(|candidate| {
                    candidate.provider_id == backend_id
                        && candidate.shared_model_id.as_deref() == Some(automatic_model)
                        && matches!(candidate.executor, ModelExecutor::Backend(_))
                })
                .collect::<Vec<_>>()
        };
        let mut candidates = matching(self.available_model_candidates());
        if candidates.is_empty() {
            candidates = matching(self.refresh_model_candidates_for(Some(selection)).await);
        }
        candidates.truncate(1);
        if candidates.is_empty() {
            return Err(EngineError::Conflict(format!(
                "backend {backend_id} no longer has buffered activity available for {selection}"
            )));
        }
        Ok(candidates)
    }

    pub(super) async fn generate_automatic_title(
        &self,
        session: &Session,
        selection: &str,
        prompt: &str,
        attachments: &[trouve_protocol::AttachmentUpload],
        shared_model: &trouve_protocol::ModelInfo,
        portable_options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<trouve_protocol::GeneratedTitle, EngineError> {
        let candidates = self
            .resolve_automatic_model_candidates(selection, None)
            .await?;
        let mut failures = Vec::new();
        for route in candidates.into_iter().take(MAX_ROUTE_ATTEMPTS_PER_TURN) {
            let route_name = route.concrete_selection_id();
            let model_options = model_options_for_schema(portable_options, &route.info);
            let result = match route.executor {
                ModelExecutor::Native(provider) => {
                    self.generate_title_with_provider(
                        prompt,
                        attachments,
                        shared_model.supports_images,
                        &model_options,
                        provider,
                        route.provider_model,
                        SESSION_TITLE_TIMEOUT,
                    )
                    .await
                }
                ModelExecutor::Backend(backend) => {
                    self.generate_title_with_backend(
                        session,
                        prompt,
                        attachments,
                        shared_model.supports_images,
                        &model_options,
                        backend,
                        route.provider_model,
                    )
                    .await
                }
            };
            match result {
                Ok(title) => return Ok(title),
                Err(error) => failures.push(format!("{route_name}: {error}")),
            }
        }
        Err(EngineError::BadRequest(format!(
            "automatic naming with {selection} failed after trying {} route(s): {}",
            failures.len(),
            failures.join("; ")
        )))
    }

    async fn rank_model_candidates(
        &self,
        selection: &str,
        mut candidates: Vec<ModelCandidate>,
        affinity: Option<&(String, String)>,
    ) -> Result<Vec<ModelCandidate>, EngineError> {
        let scheduler_cooling = |candidate: &ModelCandidate| {
            self.turn_scheduler
                .cooldown_remaining(&candidate.provider_id)
        };
        if candidates
            .iter()
            .all(|candidate| scheduler_cooling(candidate).is_some())
        {
            let retry_after = candidates
                .iter()
                .filter_map(&scheduler_cooling)
                .min()
                .unwrap_or_default()
                .as_secs()
                .max(1);
            return Err(EngineError::Conflict(format!(
                "no provider for {selection} is currently available; all providers are backing off; retry in {retry_after} seconds"
            )));
        }
        candidates.retain(|candidate| scheduler_cooling(candidate).is_none());

        let learned = self.store.route_health().map_err(EngineError::Internal)?;
        let now = chrono::Utc::now().timestamp();
        let cooling = |candidate: &ModelCandidate| {
            learned
                .get(&(
                    candidate.provider_id.clone(),
                    candidate.provider_model.clone(),
                ))
                .and_then(|health| health.retry_after)
                .is_some_and(|retry_after| retry_after > now)
        };
        if candidates.iter().all(&cooling) {
            let retry_after = candidates
                .iter()
                .filter_map(|candidate| {
                    learned
                        .get(&(
                            candidate.provider_id.clone(),
                            candidate.provider_model.clone(),
                        ))
                        .and_then(|health| health.retry_after)
                })
                .min()
                .unwrap_or(now);
            return Err(EngineError::Conflict(format!(
                "no provider for {selection} is currently available; all routes are cooling down; retry in {} seconds",
                retry_after.saturating_sub(now).max(1)
            )));
        }
        candidates.retain(|candidate| !cooling(candidate));

        let provider_order = self.config.lock().unwrap().provider_order.clone();
        let preference: HashMap<&str, usize> = provider_order
            .iter()
            .enumerate()
            .map(|(index, provider)| (provider.as_str(), index))
            .collect();
        let mut scored = candidates
            .into_iter()
            .map(|candidate| {
                let rank = match &candidate.executor {
                    ModelExecutor::Native(_) => (1u8, 0i64),
                    ModelExecutor::Backend(_) => self
                        .subscription_health_cache
                        .lock()
                        .unwrap()
                        .lookup(&candidate.provider_id, Instant::now())
                        .as_ref()
                        .map(subscription_health_rank)
                        .unwrap_or((1, 0)),
                };
                let preferred = preference.get(candidate.provider_id.as_str()).copied();
                let route = learned.get(&(
                    candidate.provider_id.clone(),
                    candidate.provider_model.clone(),
                ));
                let last_success = route.and_then(|health| health.last_success_at);
                let sticky = affinity.is_some_and(|(provider_id, provider_model)| {
                    candidate.provider_id == *provider_id
                        && candidate.provider_model == *provider_model
                        && rank.0 < 2
                });
                let score = (
                    u8::from(!sticky),
                    u8::from(rank.0 >= 2),
                    u8::from(preferred.is_none()),
                    preferred.unwrap_or(usize::MAX),
                    rank.0,
                    rank.1,
                    u8::from(last_success.is_none()),
                    last_success.map(|timestamp| -timestamp).unwrap_or(i64::MAX),
                );
                (score, rank, candidate)
            })
            .collect::<Vec<_>>();
        if scored.iter().all(|(_, rank, _)| rank.0 >= 3) {
            return Err(EngineError::Conflict(format!(
                "no provider for {selection} currently has remaining usage"
            )));
        }
        scored.retain(|(_, rank, _)| rank.0 < 3);
        scored.sort_by(|(score_a, _, a), (score_b, _, b)| {
            score_a
                .cmp(score_b)
                .then_with(|| a.provider_id.cmp(&b.provider_id))
                .then_with(|| a.provider_model.cmp(&b.provider_model))
        });
        Ok(scored
            .into_iter()
            .map(|(_, _, candidate)| candidate)
            .collect())
    }

    pub(super) async fn resolve_automatic_model_info(
        &self,
        model: &str,
    ) -> Result<Option<trouve_protocol::ModelInfo>, EngineError> {
        let Some(automatic_model) = automatic_model_name(model) else {
            return Ok(None);
        };
        let catalog_id = format!("auto/{automatic_model}");
        let matching = |candidates: Vec<ModelCandidate>| {
            candidates
                .into_iter()
                .filter(|candidate| {
                    candidate.automatic_selection_id().as_deref() == Some(&catalog_id)
                })
                .collect::<Vec<_>>()
        };
        let mut candidates = matching(self.available_model_candidates());
        if candidates.is_empty() {
            candidates = matching(
                tokio::time::timeout(
                    MODEL_CATALOG_VALIDATION_TIMEOUT,
                    self.refresh_model_candidates_for(Some(model)),
                )
                .await
                .map_err(|_| {
                    EngineError::BadRequest(format!("timed out loading model metadata for {model}"))
                })?,
            );
        }
        if candidates.is_empty() {
            return Err(EngineError::BadRequest(format!(
                "model {model} is not available from any configured provider"
            )));
        }
        Ok(Some(model_info_for_routed_selection(routed_model_info(
            catalog_id, candidates,
        ))))
    }

    fn provider_registry_snapshot(&self) -> ProviderRegistrySnapshot {
        let generations = self.provider_generations.lock().unwrap();
        let providers = self
            .providers
            .read()
            .unwrap()
            .iter()
            .map(|(id, provider)| {
                (
                    id.clone(),
                    generations.get(id).copied().unwrap_or(0),
                    provider.clone(),
                )
            })
            .collect();
        let backends = self
            .backends
            .read()
            .unwrap()
            .iter()
            .map(|(id, backend)| {
                (
                    id.clone(),
                    generations.get(id).copied().unwrap_or(0),
                    backend.clone(),
                )
            })
            .collect();
        (providers, backends)
    }

    pub(super) fn with_current_provider_generation<T>(
        &self,
        id: &str,
        expected: u64,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<Option<T>> {
        let generations = self.provider_generations.lock().unwrap();
        if generations.get(id).copied().unwrap_or(0) != expected {
            return Ok(None);
        }
        operation().map(Some)
    }

    /// Clear persistent and process-local failure state while publishing a
    /// replacement route. Holding the generation lock across the SQLite
    /// delete prevents an older in-flight attempt from repopulating stale
    /// health in the gap.
    pub(super) fn invalidate_provider_route_state(&self, id: &str) -> Result<()> {
        let mut generations = self.provider_generations.lock().unwrap();
        self.store.clear_route_health(id)?;
        let generation = generations.entry(id.to_string()).or_default();
        *generation = generation
            .checked_add(1)
            .context("provider generation exhausted")?;
        self.turn_scheduler.reset_provider_outcomes(id);
        Ok(())
    }

    pub(super) async fn run_routed_turn(
        self: &Arc<Self>,
        thread: &Thread,
        turn: u64,
        prompt: &trouve_protocol::QueuedPrompt,
        cancel: tokio_util::sync::CancellationToken,
        prompt_persisted: &AtomicBool,
        active_attempt: &Mutex<Option<RoutedAttemptSnapshot>>,
    ) -> Result<()> {
        let content = prompt.content.clone();
        let attachments = prompt.attachments.clone();
        let tools_enabled = self.store.queued_prompt_tools_enabled(&prompt.id)?;
        let session = self
            .store
            .session(&thread.session_id)?
            .context("session vanished")?;
        let workspace = self
            .store
            .workspace(&session.workspace_id)?
            .context("workspace vanished")?;
        let scope = Scope::Thread(thread.id.clone());
        let worktree = PathBuf::from(&session.worktree_path);
        let canonical_worktree = worktree.canonicalize()?;
        let tool_ctx = ToolCtx {
            cancel: cancel.clone(),
            worktree: worktree.clone(),
            canonical_worktree: Some(canonical_worktree),
            read_only_roots: crate::skills::trusted_read_roots(
                self.config_dir.as_deref(),
                Some(Path::new(&workspace.path)),
            )
            .into(),
            thread_id: thread.id.clone(),
            todos: Arc::new(Mutex::new(thread.todos.clone())),
            config_dir: self.config_dir.clone(),
            workspace_root: Some(PathBuf::from(&workspace.path)),
            edit_strategy: edit_strategy_for_model(&thread.model),
        };

        let background = self.store.is_code_review_thread(&thread.id)?;
        let personas = self.resolve_personas(Some(Path::new(&workspace.path)))?;
        let mut mode = personas::find_persona(&personas, &thread.mode)
            .cloned()
            .unwrap_or_else(personas::fallback_persona);
        if background {
            mode = personas::secure_automated_review_persona(mode);
        }

        // Publish the visible turn shell before route discovery. The provider
        // transcript remains untouched until attachment materialization has
        // succeeded, matching the concrete-turn startup boundary.
        if !prompt_persisted.load(Ordering::Acquire) {
            self.store
                .append_events_async(
                    scope.clone(),
                    self.turn_shell_events(thread, turn, prompt, false)?,
                )
                .await?;
            prompt_persisted.store(true, Ordering::Release);
        }

        let background_backend_id = if prompt.background {
            Some(
                if let Some(backend_id) = background_attach_backend_id(&content) {
                    backend_id.to_string()
                } else {
                    self.store
                        .thread_route_affinity(&thread.id)?
                        .map(|(provider_id, _)| provider_id)
                        .context("automatic background activity lost its backend route")?
                },
            )
        } else {
            None
        };

        let mut candidates = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            candidates = async {
                if let Some(backend_id) = background_backend_id.as_deref() {
                    self.resolve_background_attach_candidate(&thread.model, backend_id).await
                } else {
                    self.resolve_model_candidates(thread).await
                }
            } => {
                candidates.map_err(|error| anyhow!(error.to_string()))?
            }
        };
        anyhow::ensure!(!candidates.is_empty(), "model route disappeared");
        if background {
            candidates.retain(|candidate| match &candidate.executor {
                ModelExecutor::Native(_) => true,
                ModelExecutor::Backend(backend) if tools_enabled => {
                    self.full_tool_bridge_available_for(&candidate.provider_id)
                        || backend.confines_read_only_turns()
                }
                ModelExecutor::Backend(backend) => {
                    backend.supports_tool_free_turns() || backend.confines_read_only_turns()
                }
            });
            anyhow::ensure!(
                !candidates.is_empty(),
                "no provider route for {} can satisfy automated code review's secure tool boundary",
                thread.model
            );
        }
        let selection_info = model_info_for_routed_selection(routed_model_info(
            thread.model.clone(),
            candidates.clone(),
        ));
        let total_candidates = candidates.len();
        // Session lifecycle always precedes turn/provider capacity. Concrete
        // and automatic routes therefore share one lock order and cannot
        // deadlock while session deletion or restore waits for the write lock.
        let session_lifecycle = self.session_lock(&session.id);
        let _session_lifecycle_guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            guard = session_lifecycle.read() => guard,
        };
        let mut first_candidate = 0;
        let mut route_admission = Some(if prompt.background {
            let route = candidates
                .first()
                .context("background activity route disappeared")?;
            let capacity_model = format!("{}/{}", route.provider_id, route.provider_model);
            self.turn_scheduler.admit(&capacity_model, &cancel).await?
        } else {
            loop {
                let route = candidates.get(first_candidate).with_context(|| {
                    format!(
                        "no provider is currently able to run {}; every eligible route is cooling down",
                        thread.model
                    )
                })?;
                let capacity_model = format!("{}/{}", route.provider_id, route.provider_model);
                if self
                    .turn_scheduler
                    .cooldown_remaining(&capacity_model)
                    .is_none()
                {
                    break self.turn_scheduler.admit(&capacity_model, &cancel).await?;
                }
                first_candidate += 1;
            }
        });
        if first_candidate > 0 {
            candidates.drain(..first_candidate);
        }
        candidates.truncate(MAX_ROUTE_ATTEMPTS_PER_TURN);
        let first_route = candidates.first().context("model route disappeared")?;
        self.publish_turn_admission(
            thread,
            turn,
            background,
            route_admission
                .as_ref()
                .context("initial model route was not admitted")?
                .provider_wait_ms,
        )
        .await?;

        let has_native = candidates
            .iter()
            .any(|candidate| matches!(candidate.executor, ModelExecutor::Native(_)));
        let failover_context_window = candidates
            .iter()
            .map(|candidate| candidate.info.context_window)
            .filter(|window| *window > 0)
            .min();
        self.store.append_event(
            scope.clone(),
            Event::ModelRouteSelected {
                turn,
                model: thread.model.clone(),
                provider_id: first_route.provider_id.clone(),
                provider_model: first_route.provider_model.clone(),
                reason: trouve_protocol::ModelRouteReason::Initial,
            },
        )?;
        // Compaction summarizes only earlier transcript rows and preserves
        // the accepted current user message as the final row. If a backend is
        // selected first and later hands off to native execution, the native
        // route uses the full persisted transcript for that exceptional
        // continuation.
        if let Some(native_route) = candidates
            .iter()
            .find(|candidate| matches!(candidate.executor, ModelExecutor::Native(_)))
            && let ModelExecutor::Native(provider) = &native_route.executor
            && let Err(error) = self
                .maybe_compact(
                    thread,
                    turn,
                    provider,
                    &native_route.provider_model,
                    failover_context_window,
                    &cancel,
                )
                .await
        {
            tracing::warn!("compaction failed for {}: {error}", thread.id);
        }
        // Materialization stays behind ToolExecutor and happens once before
        // any cross-adapter handoff, so every route sees the same safe paths.
        let materialized = self
            .materialize_attachments_for_turn(&session, &attachments, &cancel)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        // Capture earlier history before appending this turn. Backends receive
        // the current prompt separately on the first attempt, while a later
        // failover rebuilds its handoff from the now-complete transcript.
        let history_before = self.store.messages(&thread.id)?;
        let (images, files): (Vec<_>, Vec<_>) = materialized
            .into_iter()
            .partition(|file| file.attachment.mime.starts_with("image/"));
        let backend_files = files
            .iter()
            .map(|file| (file.attachment.clone(), file.relative_path.clone()))
            .collect::<Vec<_>>();
        let backend_content = annotate_attachments(content, &backend_files);
        let transcript_files = images
            .iter()
            .chain(files.iter())
            .map(|file| (file.attachment.clone(), file.relative_path.clone()))
            .collect::<Vec<_>>();
        let transcript_content = annotate_attachments(prompt.content.clone(), &transcript_files);
        let backend_attachments: Vec<trouve_agents::TurnAttachment> = images
            .into_iter()
            .map(|file| trouve_agents::TurnAttachment {
                name: file.attachment.name,
                mime: file.attachment.mime,
                bytes: file.bytes,
                local_path: Some(file.absolute_path),
            })
            .collect();
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::User(transcript_content))?,
        )?;
        if !self.store.finish_queued_prompt(&prompt.id)? {
            bail!("queued prompt {} vanished before turn start", prompt.id);
        }
        self.emit_queue(&thread.id)?;

        let mut specs = Vec::new();
        if has_native && tools_enabled {
            specs = tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("turn cancelled"),
                specs = self.executor.specs(&tool_ctx) => specs,
            }
            .into_iter()
            .filter(|spec| personas::tool_allowed(&mode, &spec.name))
            .collect();
            if personas::tool_allowed(&mode, "ask_question") {
                specs.push(ask_question_spec());
            }
            if personas::tool_allowed(&mode, "search_transcript") {
                specs.push(search_transcript_spec());
            }
            if self.thread_can_spawn_subagents(&thread.id)? {
                if personas::tool_allowed(&mode, "spawn_thread") {
                    specs.push(spawn_thread_spec());
                }
                if personas::tool_allowed(&mode, "spawn_session") {
                    specs.push(spawn_session_spec());
                }
                if personas::tool_allowed(&mode, "spawn_output") {
                    specs.push(spawn_output_spec());
                }
            }
        }
        let mut system = context::system_prompt(
            &mode,
            self.config_dir.as_deref(),
            Path::new(&workspace.path),
        );
        if background {
            personas::append_automated_review_guidance(&mut system);
        }
        let stored_model_options = model_options_for_schema(
            &self.store.thread_model_options(&thread.id)?,
            &selection_info,
        );
        let permission = backend_permission_policy(tools_enabled, mode.read_only);
        // Resolve repository identity only if a vendor actually attempts to
        // create a pull request, matching the concrete backend path.
        let mut github_repository = None;
        let mut accounting = TurnAccounting {
            model: thread.model.clone(),
            ..TurnAccounting::default()
        };
        let mut native_iterations_left = MAX_ITERATIONS;
        let mut attempted_candidates = 0usize;
        let mut failover_reason = None;
        let mut last_failure = None::<(String, String, String)>;

        for (route_index, route) in candidates.iter().enumerate() {
            if route_index > 0 {
                // One admission lifetime belongs to one concrete attempt.
                // Today's value is wait telemetry; this explicit handoff also
                // prevents a future capacity lease from overlapping routes.
                let _previous_admission = route_admission.take();
                let capacity_model = format!("{}/{}", route.provider_id, route.provider_model);
                if self
                    .turn_scheduler
                    .cooldown_remaining(&capacity_model)
                    .is_some()
                {
                    continue;
                }
                route_admission = Some(self.turn_scheduler.admit(&capacity_model, &cancel).await?);
                self.store.append_event(
                    scope.clone(),
                    Event::ModelRouteSelected {
                        turn,
                        model: thread.model.clone(),
                        provider_id: route.provider_id.clone(),
                        provider_model: route.provider_model.clone(),
                        reason: failover_reason
                            .unwrap_or(trouve_protocol::ModelRouteReason::RouteFailover),
                    },
                )?;
            }
            let retrying = route_index > 0;
            attempted_candidates += 1;
            let attempt_order = route_admission
                .as_ref()
                .context("model route attempt was not admitted")?
                .attempt_order;
            *active_attempt.lock().unwrap() = Some(RoutedAttemptSnapshot {
                provider_id: route.provider_id.clone(),
                provider_model: route.provider_model.clone(),
                provider_generation: route.provider_generation,
                attempt_order,
            });
            let result = match &route.executor {
                ModelExecutor::Native(_) => {
                    self.run_native_route(
                        &session,
                        thread,
                        turn,
                        &mode,
                        &tool_ctx,
                        route,
                        &specs,
                        &system,
                        &stored_model_options,
                        retrying,
                        &mut native_iterations_left,
                        &mut accounting,
                        tools_enabled,
                        &cancel,
                    )
                    .await
                }
                ModelExecutor::Backend(_) => {
                    self.run_backend_route(
                        &session,
                        thread,
                        turn,
                        &mode,
                        route,
                        &backend_content,
                        &backend_attachments,
                        &history_before,
                        &stored_model_options,
                        retrying,
                        permission,
                        &mut github_repository,
                        &mut accounting,
                        tools_enabled,
                        prompt.background,
                        &cancel,
                    )
                    .await
                }
            };
            active_attempt.lock().unwrap().take();
            let result = result?;
            let result = if cancel.is_cancelled() {
                RouteAttemptResult::Cancelled
            } else {
                result
            };

            match result {
                RouteAttemptResult::Completed => {
                    self.with_current_provider_generation(
                        &route.provider_id,
                        route.provider_generation,
                        || {
                            self.turn_scheduler.record_ordered_outcome(
                                &route.concrete_selection_id(),
                                None,
                                attempt_order,
                            );
                            self.store.record_route_success(
                                &route.provider_id,
                                &route.provider_model,
                                attempt_order,
                            )?;
                            if automatic_model_name(&thread.model).is_some() {
                                self.store.set_thread_route_affinity(
                                    &thread.id,
                                    &thread.model,
                                    &route.provider_id,
                                    &route.provider_model,
                                )?;
                            }
                            Ok(())
                        },
                    )?;
                    self.record_routed_usage(&session.id, &thread.id, turn, &mut accounting, true)?;
                    let checkpoint_id = if mode.read_only {
                        None
                    } else {
                        self.maybe_checkpoint(&session, thread, turn, &cancel)
                            .await?
                    };
                    self.store.append_event(
                        scope,
                        Event::TurnCompleted {
                            turn,
                            usage: accounting.usage,
                            checkpoint_id,
                        },
                    )?;
                    return Ok(());
                }
                RouteAttemptResult::Cancelled => {
                    self.record_routed_usage(
                        &session.id,
                        &thread.id,
                        turn,
                        &mut accounting,
                        false,
                    )?;
                    return Ok(());
                }
                RouteAttemptResult::Failed(failure) => {
                    last_failure = Some((
                        route.provider_id.clone(),
                        route.provider_model.clone(),
                        failure.message.clone(),
                    ));
                    let (base, max) = failure.kind.cooldown();
                    let health = self.with_current_provider_generation(
                        &route.provider_id,
                        route.provider_generation,
                        || {
                            self.turn_scheduler.record_ordered_outcome(
                                &route.concrete_selection_id(),
                                Some(&failure.message),
                                attempt_order,
                            );
                            self.store
                                .record_route_failure(
                                    &route.provider_id,
                                    &route.provider_model,
                                    attempt_order,
                                    base,
                                    max,
                                )
                                .and_then(|health| {
                                    self.store.clear_thread_route_affinity_if_matches(
                                        &thread.id,
                                        &route.provider_id,
                                        &route.provider_model,
                                    )?;
                                    Ok(health)
                                })
                        },
                    )?;
                    if let Some(health) = health {
                        tracing::warn!(
                            model = %thread.model,
                            provider = %route.provider_id,
                            failures = health.consecutive_failures,
                            retry_after = health.retry_after,
                            error = %failure.message,
                            "model route opened its circuit"
                        );
                    }
                    let has_next = route_index + 1 < candidates.len();
                    if !failure.safe_to_retry {
                        self.record_routed_usage(
                            &session.id,
                            &thread.id,
                            turn,
                            &mut accounting,
                            false,
                        )?;
                        if automatic_model_name(&thread.model).is_some() && has_next {
                            bail!(
                                "automatic model {} failed on {}/{} and cannot safely switch providers: {}",
                                thread.model,
                                route.provider_id,
                                route.provider_model,
                                failure.message,
                            );
                        }
                        bail!(
                            "selected provider route {}/{} failed: {}",
                            route.provider_id,
                            route.provider_model,
                            failure.message,
                        );
                    }
                    failover_reason = Some(failure.kind.failover_reason());
                    if !has_next {
                        self.record_routed_usage(
                            &session.id,
                            &thread.id,
                            turn,
                            &mut accounting,
                            false,
                        )?;
                        let untried = total_candidates.saturating_sub(attempted_candidates);
                        if automatic_model_name(&thread.model).is_some() {
                            if untried == 0 {
                                bail!(
                                    "no provider is currently able to run {}; tried {} route(s). Last error from {}/{}: {}",
                                    thread.model,
                                    attempted_candidates,
                                    route.provider_id,
                                    route.provider_model,
                                    failure.message,
                                );
                            }
                            bail!(
                                "unable to route {} after {} route attempts; {} alternate route(s) remain deferred until the next turn. Last error from {}/{}: {}",
                                thread.model,
                                attempted_candidates,
                                untried,
                                route.provider_id,
                                route.provider_model,
                                failure.message,
                            );
                        }
                        bail!(
                            "selected provider route {}/{} failed: {}",
                            route.provider_id,
                            route.provider_model,
                            failure.message,
                        );
                    }
                }
            }
        }

        self.record_routed_usage(&session.id, &thread.id, turn, &mut accounting, false)?;
        if let Some((provider_id, provider_model, message)) = last_failure {
            bail!(
                "no provider is currently able to run {}; tried {} route(s). Last error from {}/{}: {}",
                thread.model,
                attempted_candidates,
                provider_id,
                provider_model,
                message,
            );
        }
        bail!(
            "no provider is currently able to run {}; every remaining route is cooling down",
            thread.model
        )
    }

    fn record_routed_usage(
        &self,
        session_id: &str,
        thread_id: &str,
        turn: u64,
        accounting: &mut TurnAccounting,
        record_empty: bool,
    ) -> Result<()> {
        accounting.finalize_cost();
        if record_empty
            || accounting.usage.input_tokens > 0
            || accounting.usage.output_tokens > 0
            || accounting.usage.cached_input_tokens > 0
            || accounting.usage.cost_usd.is_some()
        {
            self.store.record_usage(
                session_id,
                thread_id,
                turn,
                &accounting.model,
                &accounting.usage,
                accounting.context_input_tokens,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_native_route(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
        tool_ctx: &ToolCtx,
        route: &ModelCandidate,
        specs: &[ToolSpec],
        system: &str,
        stored_model_options: &serde_json::Map<String, serde_json::Value>,
        retrying: bool,
        iterations_left: &mut usize,
        accounting: &mut TurnAccounting,
        tools_enabled: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<RouteAttemptResult> {
        let ModelExecutor::Native(provider) = &route.executor else {
            unreachable!("native route helper received a backend")
        };
        let scope = Scope::Thread(thread.id.clone());
        let model_options = model_options_for_schema(stored_model_options, &route.info);
        // Keep one sanitized transcript in memory for the provider tool loop.
        // Every assistant/tool message is still persisted immediately, then
        // appended here for the next iteration without re-reading and
        // deserializing the entire thread.
        let mut messages = vec![Message::System(system.to_string())];
        for payload in self.store.messages(&thread.id)? {
            messages.push(serde_json::from_value(payload)?);
        }
        let mut messages = sanitize_transcript(messages);
        if retrying {
            messages.push(Message::User(
                "Another provider could not continue this turn. Continue the in-progress \
                 response from the transcript and current worktree without repeating \
                 completed text, tool calls, or edits."
                    .into(),
            ));
        }
        let mut side_effect_started = false;

        while *iterations_left > 0 {
            if cancel.is_cancelled() {
                return Ok(RouteAttemptResult::Cancelled);
            }
            *iterations_left -= 1;

            let mut text = String::new();
            let mut tool_calls = Vec::new();
            let mut reasoning = Vec::new();
            let attempt_error = match provider
                .stream_chat(&route.provider_model, &messages, specs, &model_options)
                .await
            {
                Err(error) => Some(error),
                Ok(stream) => {
                    let mut stream = trouve_providers::coalesce_event_stream(stream);
                    let mut error = None;
                    let mut completed = false;
                    loop {
                        let event = tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break,
                            event = stream.next() => match event {
                                Some(event) => event,
                                None => break,
                            },
                        };
                        match event {
                            Err(provider_error) => {
                                error = Some(provider_error);
                                break;
                            }
                            Ok(ProviderEvent::TextDelta(delta)) => {
                                text.push_str(&delta);
                                self.store.append_event(
                                    scope.clone(),
                                    Event::AssistantDelta { turn, text: delta },
                                )?;
                            }
                            Ok(ProviderEvent::ThinkingStarted { .. }) => {}
                            Ok(ProviderEvent::ThinkingDelta { id, text }) => {
                                self.store.append_event(
                                    scope.clone(),
                                    Event::AssistantThinking {
                                        turn,
                                        id: Some(id),
                                        text,
                                    },
                                )?;
                            }
                            Ok(ProviderEvent::ThinkingCompleted { id }) => {
                                self.store.append_event(
                                    scope.clone(),
                                    Event::AssistantThinkingCompleted { turn, id: Some(id) },
                                )?;
                            }
                            Ok(ProviderEvent::Reasoning(block)) => reasoning.push(block),
                            Ok(ProviderEvent::ToolCall(call)) => tool_calls.push(call),
                            Ok(ProviderEvent::Completed { usage }) => {
                                completed = true;
                                accounting.add_native(&self.model_catalog, route, &usage);
                            }
                        }
                    }
                    if error.is_none() && !cancel.is_cancelled() && !completed {
                        error = Some(trouve_providers::ProviderError::Request(
                            "provider stream ended before a completion event".into(),
                        ));
                    }
                    error
                }
            };

            if let Some(error) = attempt_error {
                if !text.is_empty() {
                    self.store.append_event(
                        scope.clone(),
                        Event::AssistantMessage {
                            turn,
                            content: text.clone(),
                        },
                    )?;
                }
                if !text.is_empty() || !reasoning.is_empty() {
                    self.store.append_message(
                        &thread.id,
                        &serde_json::to_value(Message::Assistant {
                            content: text,
                            tool_calls: Vec::new(),
                            reasoning,
                        })?,
                    )?;
                }
                return Ok(RouteAttemptResult::Failed(native_attempt_failure(
                    error,
                    side_effect_started,
                )));
            }

            if cancel.is_cancelled() {
                if !text.is_empty() {
                    self.store.append_event(
                        scope.clone(),
                        Event::AssistantMessage {
                            turn,
                            content: text.clone(),
                        },
                    )?;
                }
                if !text.is_empty() || !reasoning.is_empty() {
                    self.store.append_message(
                        &thread.id,
                        &serde_json::to_value(Message::Assistant {
                            content: text,
                            tool_calls: Vec::new(),
                            reasoning,
                        })?,
                    )?;
                }
                return Ok(RouteAttemptResult::Cancelled);
            }

            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
            }
            if !tools_enabled && !tool_calls.is_empty() {
                tracing::warn!(
                    thread_id = %thread.id,
                    turn,
                    "provider requested a tool during a tool-free turn; ignoring the request"
                );
                tool_calls.clear();
            }
            if !text.is_empty() || !tool_calls.is_empty() {
                let assistant = Message::Assistant {
                    content: text,
                    tool_calls: tool_calls.clone(),
                    reasoning,
                };
                self.store
                    .append_message(&thread.id, &serde_json::to_value(&assistant)?)?;
                messages.push(assistant);
            }
            if tool_calls.is_empty() {
                return Ok(RouteAttemptResult::Completed);
            }
            // Classify before dispatch: after a mutation-capable call begins,
            // its outcome may be unknown if the next provider request fails.
            // Engine-served read/question helpers are the only calls outside
            // ToolExecutor that are known not to change durable state.
            side_effect_started |= tool_calls.iter().any(|call| match call.name.as_str() {
                "ask_question" | "search_transcript" | "spawn_output" => false,
                "spawn_thread" | "spawn_session" => true,
                _ => self.executor.tool_mutates(&call.name) != Some(false),
            });
            // Keep the same read-only concurrency and mutation barriers used
            // by concrete-model turns. Route failover must not change tool
            // scheduling semantics merely because the model was automatic.
            let results = self
                .handle_tool_calls_parallel(
                    session, thread, turn, mode, tool_ctx, tool_calls, cancel,
                )
                .await;
            for (call_id, result) in results {
                let (result_content, images) = result?;
                let result = Message::ToolResult {
                    call_id,
                    content: result_content,
                    images,
                };
                self.store
                    .append_message(&thread.id, &serde_json::to_value(&result)?)?;
                messages.push(result);
            }
        }

        self.run_native_iteration_summary(
            thread,
            turn,
            route,
            system,
            &model_options,
            accounting,
            side_effect_started,
            cancel,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_native_iteration_summary(
        &self,
        thread: &Thread,
        turn: u64,
        route: &ModelCandidate,
        system: &str,
        model_options: &serde_json::Map<String, serde_json::Value>,
        accounting: &mut TurnAccounting,
        side_effect_started: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<RouteAttemptResult> {
        let ModelExecutor::Native(provider) = &route.executor else {
            unreachable!("native summary helper received a backend")
        };
        let scope = Scope::Thread(thread.id.clone());
        let mut messages = vec![Message::System(system.to_string())];
        for payload in self.store.messages(&thread.id)? {
            messages.push(serde_json::from_value(payload)?);
        }
        let mut messages = sanitize_transcript(messages);
        messages.push(Message::User(format!(
            "You reached the hard {MAX_ITERATIONS}-step limit for this turn. Do not call any \
             more tools. Give the user a concise progress report based on the tool results \
             above, clearly identify unfinished work, and ask them to continue in a new turn."
        )));
        let mut text = String::new();
        let mut reasoning = Vec::new();
        let error = match provider
            .stream_chat(&route.provider_model, &messages, &[], model_options)
            .await
        {
            Err(error) => Some(error),
            Ok(stream) => {
                let mut stream = trouve_providers::coalesce_event_stream(stream);
                let mut error = None;
                let mut completed = false;
                loop {
                    let event = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        event = stream.next() => match event {
                            Some(event) => event,
                            None => break,
                        },
                    };
                    match event {
                        Ok(ProviderEvent::TextDelta(delta)) => {
                            text.push_str(&delta);
                            self.store.append_event(
                                scope.clone(),
                                Event::AssistantDelta { turn, text: delta },
                            )?;
                        }
                        Ok(ProviderEvent::ThinkingStarted { .. }) => {}
                        Ok(ProviderEvent::ThinkingDelta { id, text }) => {
                            self.store.append_event(
                                scope.clone(),
                                Event::AssistantThinking {
                                    turn,
                                    id: Some(id),
                                    text,
                                },
                            )?;
                        }
                        Ok(ProviderEvent::ThinkingCompleted { id }) => {
                            self.store.append_event(
                                scope.clone(),
                                Event::AssistantThinkingCompleted { turn, id: Some(id) },
                            )?;
                        }
                        Ok(ProviderEvent::Reasoning(block)) => reasoning.push(block),
                        Ok(ProviderEvent::Completed { usage }) => {
                            completed = true;
                            accounting.add_native(&self.model_catalog, route, &usage);
                        }
                        Ok(ProviderEvent::ToolCall(_)) => {}
                        Err(provider_error) => {
                            error = Some(provider_error);
                            break;
                        }
                    }
                }
                if error.is_none() && !cancel.is_cancelled() && !completed {
                    error = Some(trouve_providers::ProviderError::Request(
                        "provider stream ended before a completion event".into(),
                    ));
                }
                error
            }
        };
        if cancel.is_cancelled() {
            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
            }
            if !text.is_empty() || !reasoning.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning,
                    })?,
                )?;
            }
            return Ok(RouteAttemptResult::Cancelled);
        }
        if let Some(error) = error {
            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
            }
            if !text.is_empty() || !reasoning.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning,
                    })?,
                )?;
            }
            return Ok(RouteAttemptResult::Failed(native_attempt_failure(
                error,
                side_effect_started,
            )));
        }
        if text.trim().is_empty() {
            text = format!(
                "Reached the {MAX_ITERATIONS}-step limit for one turn and stopped mid-task. \
                 Send another message to continue."
            );
        }
        self.store.append_event(
            scope,
            Event::AssistantMessage {
                turn,
                content: text.clone(),
            },
        )?;
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::Assistant {
                content: text,
                tool_calls: Vec::new(),
                reasoning,
            })?,
        )?;
        Ok(RouteAttemptResult::Completed)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_backend_route(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
        route: &ModelCandidate,
        initial_content: &str,
        attachments: &[trouve_agents::TurnAttachment],
        history_before: &[serde_json::Value],
        stored_model_options: &serde_json::Map<String, serde_json::Value>,
        retrying: bool,
        permission: BackendPermission,
        github_repository: &mut Option<(String, String, String)>,
        accounting: &mut TurnAccounting,
        tools_enabled: bool,
        attach_background: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<RouteAttemptResult> {
        let ModelExecutor::Backend(backend) = &route.executor else {
            unreachable!("backend route helper received a native provider")
        };
        let effective_read_only = !tools_enabled || mode.read_only;
        // Backends that can remove every native tool must honor the strict
        // tool-free contract. Others remain usable for read/search activity,
        // but run read-only with no mounted MCP servers.
        let strict_tool_free =
            backend_strict_tool_free_policy(tools_enabled, backend.supports_tool_free_turns());
        let scope = Scope::Thread(thread.id.clone());
        let backend_id = &route.provider_id;
        let payloads = if retrying {
            self.store.messages(&thread.id)?
        } else {
            history_before.to_vec()
        };
        let submitted_transcript_messages =
            u64::try_from(payloads.len().saturating_add(usize::from(!retrying)))
                .context("backend transcript length exceeds u64")?;
        let resume = if tools_enabled {
            self.store.backend_session(&thread.id, backend_id)?
        } else {
            None
        };
        let unseen = match &resume {
            Some((_, seen)) => payloads.get(*seen as usize..).unwrap_or(&payloads),
            None => &payloads,
        };
        let handoff = {
            let messages: Vec<Message> = unseen
                .iter()
                .filter_map(|payload| serde_json::from_value(payload.clone()).ok())
                .collect();
            render_history_digest(&messages, resume.is_some())
        };
        let vendor_session = resume.map(|(id, _)| id);
        let mut active_vendor_session = vendor_session.clone();
        if let Some(vendor_session_id) = active_vendor_session.as_deref() {
            self.bridged_tool_owners
                .bind_vendor_thread(&thread.id, vendor_session_id, &thread.id)
                .map_err(anyhow::Error::msg)?;
        }
        let attempt_prompt = if retrying {
            let continuation = "Another provider could not continue this turn. Continue the \
                in-progress task from the transcript and current worktree. Do not repeat \
                completed text, commands, or edits; inspect state when unsure.";
            match handoff {
                Some(digest) => format!("{digest}\n\n{continuation}"),
                None => continuation.into(),
            }
        } else {
            match handoff {
                Some(digest) => format!("{digest}\n\n{initial_content}"),
                None => initial_content.into(),
            }
        };

        let concrete_model = route.concrete_selection_id();
        let mcp_bridge = tools_enabled
            .then(|| self.mcp_bridge_for(&concrete_model, &thread.id))
            .flatten();
        let mut instructions = mode.system_prompt.trim().to_string();
        let full_tool_bridge = mcp_bridge
            .as_ref()
            .is_some_and(|bridge| bridge.bridge_tools);
        let automated_review = self.store.is_code_review_thread(&thread.id)?;
        append_vendor_search_guidance(&mut instructions, mcp_bridge.is_some(), automated_review);
        enforce_automated_review_backend_boundary(
            automated_review,
            tools_enabled,
            full_tool_bridge,
            backend.confines_read_only_turns(),
            backend_id,
        )?;
        if full_tool_bridge {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(crate::tools::VENDOR_TOOL_BRIDGE_GUIDANCE);
        }
        // Automatic routes never mount user MCP servers directly. They are
        // exposed only through a full trouve bridge, where ToolExecutor owns
        // permissions, auditing, and the session mutation lane.
        let mcp_servers = Vec::new();
        let model_options = model_options_for_schema(stored_model_options, &route.info);
        // A routed attempt needs its own cancellation boundary. Provider
        // failover must be able to stop and acknowledge one vendor process
        // without cancelling the user-visible turn or its later routes.
        let attempt_cancel = cancel.child_token();
        let backend_turn = BackendTurn {
            cancel: attempt_cancel.clone(),
            thread_id: thread.id.clone(),
            worktree: PathBuf::from(&session.worktree_path),
            session: vendor_session,
            model: route.provider_model.clone(),
            model_options,
            prompt: attempt_prompt,
            attachments: attachments.to_vec(),
            instructions: (!instructions.is_empty()).then_some(instructions),
            permission,
            tool_free: strict_tool_free,
            attach_background,
            mcp_bridge,
            mcp_servers,
        };
        let startup_permit = match self
            .turn_scheduler
            .acquire_backend_startup(backend_id, cancel)
            .await
        {
            Ok(permit) => permit,
            Err(_) if cancel.is_cancelled() => return Ok(RouteAttemptResult::Cancelled),
            Err(error) => return Err(error),
        };
        let startup_activity = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(RouteAttemptResult::Cancelled),
            activity = backend.startup_activity(&backend_turn) => activity,
        };
        if matches!(
            startup_activity,
            Some(BackendStartupActivity::ConnectingTools)
        ) {
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::TurnPhaseChanged {
                        turn,
                        phase: TurnPhase::ConnectingTools,
                    },
                )
                .await?;
        }
        let start_backend = backend.clone();
        let mut start = Box::pin(async move { start_backend.run_turn(backend_turn).await });
        let started = tokio::select! {
            biased;
            result = &mut start => Some(result),
            _ = cancel.cancelled() => None,
        };
        let started = match started {
            Some(result) => result,
            None => {
                // Fence new mutations before asking the vendor to stop. If a
                // custom backend misses its acknowledgement deadline, retain
                // both the startup future and fence until it really exits.
                let quarantine = BackendMutationQuarantine::queue(
                    self.tool_mutation_admission_lock(&session.id),
                )
                .await;
                attempt_cancel.cancel();
                match tokio::time::timeout(BACKEND_CANCEL_CLEANUP_TIMEOUT, &mut start).await {
                    Ok(_) => return Ok(RouteAttemptResult::Cancelled),
                    Err(_) => {
                        let backend_id = backend_id.clone();
                        let thread_id = thread.id.clone();
                        tokio::spawn(async move {
                            let _quarantine = quarantine;
                            if let Ok(mut stream) = start.await {
                                while stream.next().await.is_some() {}
                            }
                            tracing::warn!(
                                backend_id,
                                thread_id,
                                "late backend startup cancellation completed; session lane released"
                            );
                        });
                        return Ok(RouteAttemptResult::Cancelled);
                    }
                }
            }
        };
        let mut stream = match started {
            Ok(stream) => stream,
            Err(BackendError::Cancelled) if cancel.is_cancelled() => {
                return Ok(RouteAttemptResult::Cancelled);
            }
            Err(error) => {
                return Ok(RouteAttemptResult::Failed(backend_attempt_failure(
                    error, false,
                )));
            }
        };
        // The vendor accepted the turn; another turn may begin its own
        // bounded startup while this stream remains active.
        drop(startup_permit);
        let post_start_persistence_error = if startup_activity.is_some() {
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::TurnPhaseChanged {
                        turn,
                        phase: TurnPhase::Processing,
                    },
                )
                .await
                .err()
        } else {
            None
        };

        let mut text = String::new();
        let mut segment = String::new();
        let mut attempt_usage = Usage::default();
        let mut backend_error = None;
        let mut attempt_error = None;
        let mut backend_cancelled = false;
        let mut backend_completed = false;
        let mut open_tools = HashSet::new();
        let mut seen_tool_cards = HashSet::new();
        let mut side_effect_started = false;
        let mut tool_calls =
            HashMap::<String, (String, serde_json::Value, PullRequestCreationRequest)>::new();
        let mut tool_started_at = HashMap::<String, Instant>::new();
        let mut github_creation_output = HashMap::<String, String>::new();
        let mut vendor_threads = HashMap::<String, String>::new();
        if let Some(vendor_session_id) = active_vendor_session.as_ref() {
            vendor_threads.insert(vendor_session_id.clone(), thread.id.clone());
        }
        let mut collaborators = HashMap::<String, BackendCollaboratorProjection>::new();
        let mut collaborator_claims = BackendCollaboratorClaims::new(&self.active_threads);
        let mut pending_backend_approvals = futures::stream::FuturesUnordered::new();
        let mut backend_approval_cancels =
            HashMap::<String, tokio_util::sync::CancellationToken>::new();
        let mut backend_mutation_permits = HashMap::<String, SessionMutationPermit>::new();
        let mut suppressed_bridge_calls = HashSet::new();
        let mut persisted = Vec::new();
        let mut persist_deadline = None;
        let event_loop_result: Result<()> = async {
          if let Some(error) = post_start_persistence_error {
              return Err(error);
          }
          loop {
            let flush_at = persist_deadline.unwrap_or_else(Instant::now);
            let input = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep_until(flush_at.into()), if persist_deadline.is_some() => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                    persist_deadline = None;
                    continue;
                }
                approval = pending_backend_approvals.next(), if !pending_backend_approvals.is_empty() => {
                    BackendLoopInput::Approval(
                        approval.expect("non-empty approval queue must yield an outcome")
                    )
                }
                event = stream.next() => BackendLoopInput::Event(event),
            };
            let event = match input {
                BackendLoopInput::Event(None) => {
                    if !backend_completed && !cancel.is_cancelled() {
                        backend_error = Some(BackendError::Protocol(
                            "backend stream ended before a completion event".into(),
                        ));
                    }
                    break;
                }
                BackendLoopInput::Approval(outcome) => {
                    let BackendApprovalOutcome {
                        owner_thread_id,
                        call_id,
                        responder,
                        approved,
                        mutation_permit,
                    } = outcome;
                    if let Some(owner_thread_id) = owner_thread_id {
                        let Some(collaborator) = collaborators
                            .values_mut()
                            .find(|collaborator| collaborator.thread.id == owner_thread_id)
                        else {
                            let _ = responder.send(false);
                            continue;
                        };
                        collaborator.approval_cancels.remove(&call_id);
                        if collaborator.terminal {
                            let _ = responder.send(false);
                            continue;
                        }
                        let approved = match approved {
                            Ok(approved) => approved,
                            Err(error) => {
                                let _ = responder.send(false);
                                attempt_error = Some(error);
                                break;
                            }
                        };
                        if approved {
                            if let Some(permit) = mutation_permit {
                                collaborator
                                    .mutation_permits
                                    .insert(call_id.clone(), permit);
                            }
                            if responder.send(true).is_err() {
                                collaborator.mutation_permits.remove(&call_id);
                            }
                        } else {
                            let _ = responder.send(false);
                        }
                        continue;
                    }
                    backend_approval_cancels.remove(&call_id);
                    let approved = match approved {
                        Ok(approved) => approved,
                        Err(error) => {
                            let _ = responder.send(false);
                            attempt_error = Some(error);
                            break;
                        }
                    };
                    if approved {
                        if let Some(permit) = mutation_permit {
                            backend_mutation_permits.insert(call_id.clone(), permit);
                        }
                        if responder.send(true).is_err() {
                            backend_mutation_permits.remove(&call_id);
                        }
                    } else {
                        let _ = responder.send(false);
                    }
                    continue;
                }
                BackendLoopInput::Event(Some(Ok(event))) => event,
                BackendLoopInput::Event(Some(Err(BackendError::Cancelled)))
                    if cancel.is_cancelled() =>
                {
                    backend_cancelled = true;
                    break;
                }
                BackendLoopInput::Event(Some(Err(error))) => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                    backend_error = Some(error);
                    break;
                }
            };
            match event {
                BackendEvent::SessionStarted { session_id } => {
                    active_vendor_session = Some(session_id.clone());
                    vendor_threads.insert(session_id.clone(), thread.id.clone());
                    self.bridged_tool_owners
                        .bind_vendor_thread(&thread.id, &session_id, &thread.id)
                        .map_err(anyhow::Error::msg)?;
                    if tools_enabled {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        self.store.set_backend_session_at_watermark(
                            &thread.id,
                            backend_id,
                            &session_id,
                            submitted_transcript_messages,
                        )?;
                    }
                }
                BackendEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    segment.push_str(&delta);
                    persisted.push(Event::AssistantDelta { turn, text: delta });
                }
                BackendEvent::ProgressDelta(delta) => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::AssistantProgress { turn, text: delta });
                }
                BackendEvent::ProgressCompleted => {
                    persisted.push(Event::AssistantProgressCompleted { turn });
                }
                BackendEvent::ThinkingDelta(delta) => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::AssistantThinking {
                        turn,
                        id: Some("reasoning".into()),
                        text: delta,
                    });
                }
                BackendEvent::ThinkingCompleted => {
                    persisted.push(Event::AssistantThinkingCompleted {
                        turn,
                        id: Some("reasoning".into()),
                    });
                }
                BackendEvent::ToolStarted {
                    call_id,
                    tool,
                    mut args,
                } => {
                    if let Some((nested_tool, _)) = trouve_bridge_wrapper_call(&tool, &args) {
                        side_effect_started |=
                            self.executor.tool_mutates(nested_tool) != Some(false);
                        if suppressed_bridge_calls.insert(call_id.clone())
                            && let Some(vendor_thread_id) = active_vendor_session.as_deref()
                        {
                            self.announce_trouve_bridge_wrapper(
                                &thread.id,
                                vendor_thread_id,
                                &thread.id,
                                &call_id,
                                &tool,
                                &args,
                            );
                        }
                        continue;
                    }
                    if strict_tool_free {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        side_effect_started = true;
                        backend_error = Some(BackendError::Protocol(format!(
                            "backend requested tool {tool} during a tool-free turn"
                        )));
                        break;
                    }
                    // First-party MCP calls reserve inside handle_tool_call;
                    // Claude mirrors them here under mcp__trouve__*. Native
                    // reads on a backend without a true tool-free mode are
                    // confined but intentionally outside the zero-call cap.
                    open_tools.insert(call_id.clone());
                    let first_start = seen_tool_cards.insert(call_id.clone());
                    if vendor_tool_uses_automated_review_budget(
                        tools_enabled,
                        &tool,
                        first_start,
                    ) {
                        self.automated_review_tool_budgets.reserve(&thread.id)?;
                    }
                    tool_started_at.insert(call_id.clone(), Instant::now());
                    side_effect_started = true;
                    let could_create = could_request_pull_request_creation(&tool, &args);
                    let mut creation_request = PullRequestCreationRequest::Rejected;
                    if could_create {
                        if let Some((_, owner, repo)) = github_repository.as_ref() {
                            creation_request =
                                classify_pull_request_creation(&tool, &args, owner, repo);
                        } else {
                            let repository = self
                                .github_repository_for_session(session)
                                .context("discovering repository for pull request creator")?;
                            let (_, owner, repo) = &repository;
                            creation_request =
                                classify_pull_request_creation(&tool, &args, owner, repo);
                            if !matches!(creation_request, PullRequestCreationRequest::Rejected) {
                                *github_repository = Some(repository);
                            }
                        }
                    }
                    tool_calls.insert(
                        call_id.clone(),
                        (tool.clone(), args.clone(), creation_request),
                    );
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    annotate_edit_lines(Path::new(&session.worktree_path), &mut args);
                    if first_start && !self.tool_card_exists(&thread.id, turn, &call_id) {
                        persisted.push(Event::ToolRequested {
                            turn,
                            call_id: call_id.clone(),
                            tool,
                            args,
                            requires_approval: false,
                        });
                    }
                    persisted.push(Event::ToolStarted { call_id });
                }
                BackendEvent::ToolOutput { call_id, chunk } => {
                    if suppressed_bridge_calls.contains(&call_id) {
                        continue;
                    }
                    if github_repository.is_some()
                        && let Some((_, _, request)) = tool_calls.get(&call_id)
                        && !matches!(request, PullRequestCreationRequest::Rejected)
                    {
                        github_creation_output
                            .entry(call_id.clone())
                            .or_default()
                            .push_str(&chunk);
                    }
                    persisted.push(Event::ToolOutput { call_id, chunk });
                }
                BackendEvent::CommandsUpdated { commands } => {
                    persisted.push(Event::CommandsUpdated { commands });
                }
                BackendEvent::TodosUpdated { todos } => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    self.store.update_thread_todos(&thread.id, &todos)?;
                    persisted.push(Event::TodosUpdated { todos });
                }
                BackendEvent::UsageUpdated { usage } => {
                    persisted.push(Event::TurnUsageUpdated { turn, usage });
                }
                BackendEvent::CompactionStarted => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::CompactionStarted { turn });
                }
                BackendEvent::CompactionCompleted => {
                    persisted.push(Event::CompactionCompleted {
                        turn,
                        messages_compacted: 0,
                    });
                }
                BackendEvent::CompactionFailed => {
                    persisted.push(Event::CompactionFailed { turn });
                }
                BackendEvent::CollaboratorStarted {
                    session_id,
                    parent_session_id,
                    name,
                    access,
                    prompt,
                    model,
                    thinking_level,
                } => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    let vendor_session_id = session_id.clone();
                    let prompt_announced =
                        prompt.as_deref().is_some_and(|prompt| !prompt.is_empty());
                    self.start_backend_collaborator_claimed(
                        session,
                        thread,
                        backend_id,
                        session_id,
                        &parent_session_id,
                        name,
                        access,
                        prompt,
                        model,
                        thinking_level,
                        &mut collaborator_claims,
                        &mut vendor_threads,
                        &mut collaborators,
                    )
                    .await?;
                    if let Some(owner_thread_id) = vendor_threads.get(&vendor_session_id) {
                        self.bridged_tool_owners
                            .bind_vendor_thread(&thread.id, &vendor_session_id, owner_thread_id)
                            .map_err(anyhow::Error::msg)?;
                    }
                    self.publish_backend_collaborator_spawn(
                        thread,
                        turn,
                        &vendor_session_id,
                        &mut collaborators,
                    )
                    .await?;
                    if prompt_announced
                        && let Some(collaborator) = collaborators.get_mut(&vendor_session_id)
                    {
                        flush_backend_event_batch(
                            &self.store,
                            &Scope::Thread(collaborator.thread.id.clone()),
                            &mut collaborator.persisted,
                        )
                        .await?;
                    }
                }
                BackendEvent::CollaboratorEvent {
                    session_id,
                    turn_id,
                    mut event,
                } => {
                    if strict_tool_free {
                        event = match event {
                            BackendCollaboratorEvent::ToolStarted { tool, .. } => {
                                side_effect_started = true;
                                backend_error = Some(BackendError::Protocol(format!(
                                    "backend collaborator requested tool {tool} during a tool-free turn"
                                )));
                                break;
                            }
                            BackendCollaboratorEvent::ApprovalNeeded {
                                tool, responder, ..
                            } => {
                                let _ = responder.send(false);
                                backend_error = Some(BackendError::Protocol(format!(
                                    "backend collaborator requested approval for {tool} during a tool-free turn"
                                )));
                                break;
                            }
                            event => event,
                        };
                    }
                    if matches!(
                        &event,
                        BackendCollaboratorEvent::ToolStarted { .. }
                            | BackendCollaboratorEvent::ApprovalNeeded { .. }
                    ) {
                        side_effect_started = true;
                    }
                    if !collaborators.contains_key(&session_id) {
                        let parent_session_id = active_vendor_session
                            .as_deref()
                            .unwrap_or_default()
                            .to_string();
                        self.start_backend_collaborator_claimed(
                            session,
                            thread,
                            backend_id,
                            session_id.clone(),
                            &parent_session_id,
                            None,
                            BackendCollaboratorAccess::Inherit,
                            None,
                            None,
                            None,
                            &mut collaborator_claims,
                            &mut vendor_threads,
                            &mut collaborators,
                        )
                        .await?;
                    }
                    if let Some(owner_thread_id) = vendor_threads.get(&session_id) {
                        self.bridged_tool_owners
                            .bind_vendor_thread(&thread.id, &session_id, owner_thread_id)
                            .map_err(anyhow::Error::msg)?;
                    }
                    if let Some(collaborator) = collaborators.get(&session_id)
                        && !collaborator_claims.claim(&collaborator.thread.id, &session.id)
                    {
                        bail!(
                            "cannot route provider collaborator {} while another turn owns it",
                            collaborator.thread.id
                        );
                    }
                    let completed_successfully =
                        matches!(&event, BackendCollaboratorEvent::Completed { .. });
                    let terminal_thread =
                        if let Some(collaborator) = collaborators.get_mut(&session_id) {
                            self.prepare_backend_collaborator_turn(
                                session,
                                backend_id,
                                collaborator,
                                turn_id.as_deref(),
                            )
                            .await?;
                            if !self.suppress_collaborator_bridge_wrapper(
                                &thread.id,
                                &session_id,
                                collaborator,
                                &event,
                            ) {
                                self.persist_backend_collaborator_event(
                                    session,
                                    mode,
                                    backend_id,
                                    collaborator,
                                    event,
                                    cancel,
                                )
                                .await?;
                            }
                            if let Some(approval) = collaborator.pending_approval.take() {
                                let owner_thread_id = approval.thread.id.clone();
                                let approval_call_id = approval.call_id.clone();
                                let approval_cancel = cancel.child_token();
                                collaborator
                                    .approval_cancels
                                    .insert(approval_call_id, approval_cancel.clone());
                                pending_backend_approvals.push(self.pending_backend_approval(
                                    session.clone(),
                                    approval.thread,
                                    approval.turn,
                                    effective_read_only || approval.mode.read_only,
                                    approval.call_id,
                                    approval.tool,
                                    approval.args,
                                    approval.responder,
                                    approval_cancel,
                                    !full_tool_bridge,
                                    Some(owner_thread_id),
                                ));
                            }
                            collaborator
                                .terminal
                                .then(|| collaborator.thread.id.clone())
                        } else {
                            None
                        };
                    self.publish_backend_collaborator_spawn(
                        thread,
                        turn,
                        &session_id,
                        &mut collaborators,
                    )
                    .await?;
                    if let Some(thread_id) = terminal_thread {
                        collaborator_claims.release(&thread_id);
                        if completed_successfully {
                            self.dispatch_queue(&thread_id)
                                .map_err(|error| anyhow!(error.to_string()))?;
                        }
                    }
                }
                BackendEvent::ToolCompleted {
                    call_id,
                    ok,
                    result,
                } => {
                    if suppressed_bridge_calls.remove(&call_id) {
                        // The bridged execution path owns persistence and PR
                        // evidence for this duplicate vendor lifecycle card.
                        backend_mutation_permits.remove(&call_id);
                        continue;
                    }
                    open_tools.remove(&call_id);
                    let status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Error
                    };
                    let execution_duration_ms =
                        tool_started_at.remove(&call_id).map(monotonic_elapsed_ms);
                    let todos = match tool_calls.get(&call_id) {
                        Some((tool, args, _)) => self.persist_todos_from_result(
                            &thread.id,
                            tool,
                            status,
                            &result,
                            Some(args),
                        )?,
                        None => None,
                    };
                    let mut verification = None;
                    if ok
                        && let Some(repository @ (host, owner, repo)) =
                            github_repository.as_ref()
                        && let Some((_, _, request)) = tool_calls.get(&call_id)
                    {
                        if !matches!(request, PullRequestCreationRequest::Rejected) {
                            let result_numbers = pr_numbers_in_value(&result, host, owner, repo);
                            let creation_output =
                                github_creation_output.remove(&call_id).unwrap_or_default();
                            let output_numbers = crate::github::pr_numbers_in_text(
                                &creation_output,
                                host,
                                owner,
                                repo,
                            );
                            if matches!(request, PullRequestCreationRequest::Confirmed)
                                || (matches!(request, PullRequestCreationRequest::Unresolved)
                                    && (!result_numbers.is_empty()
                                        || !output_numbers.is_empty()))
                            {
                                verification =
                                    Some((repository.clone(), result_numbers, output_numbers));
                            }
                        }
                        github_creation_output.remove(&call_id);
                    } else {
                        github_creation_output.remove(&call_id);
                    }
                    // Approval-gated vendor creators retain the session write
                    // lane until this post-execution attestation is captured.
                    let evidence = if verification.is_some() {
                        let evidence = if backend_mutation_permits.contains_key(&call_id) {
                            Self::capture_session_pr_head(session).await
                        } else {
                            None
                        };
                        if evidence.is_none() {
                            tracing::warn!(
                                session_id = session.id,
                                %call_id,
                                "cannot capture immutable PR ownership evidence"
                            );
                        }
                        evidence
                    } else {
                        None
                    };
                    backend_mutation_permits.remove(&call_id);

                    let mut completion_events = vec![Event::ToolCompleted {
                        call_id,
                        status,
                        result,
                        execution_duration_ms,
                    }];
                    if let Some(todos) = todos {
                        completion_events.push(Event::TodosUpdated { todos });
                    }
                    if let Some((repository, priority_numbers, fallback_numbers)) = verification {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        let intents = Self::session_pr_verification_intents(
                            session,
                            repository,
                            priority_numbers,
                            fallback_numbers,
                            evidence,
                        );
                        self.store
                            .append_events_with_session_pr_verification_intents(
                                scope.clone(),
                                completion_events,
                                intents.clone(),
                            )
                            .await?;
                        if !intents.is_empty() {
                            self.session_pr_verification_wake.notify_one();
                        }
                    } else {
                        persisted.extend(completion_events);
                    }
                }
                BackendEvent::ApprovalNeeded {
                    call_id,
                    tool,
                    args,
                    responder,
                } => {
                    if strict_tool_free {
                        let _ = responder.send(false);
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        backend_error = Some(BackendError::Protocol(format!(
                            "backend requested approval for {tool} during a tool-free turn"
                        )));
                        break;
                    }
                    side_effect_started = true;
                    open_tools.insert(call_id.clone());
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    persist_deadline = None;
                    let approval_cancel = cancel.child_token();
                    backend_approval_cancels.insert(call_id.clone(), approval_cancel.clone());
                    pending_backend_approvals.push(self.pending_backend_approval(
                        session.clone(),
                        thread.clone(),
                        turn,
                        effective_read_only,
                        call_id,
                        tool,
                        args,
                        responder,
                        approval_cancel,
                        !full_tool_bridge,
                        None,
                    ));
                    continue;
                }
                BackendEvent::QuestionsNeeded {
                    request_id,
                    title,
                    questions,
                    responder,
                } => {
                    // Vendor question extensions are another engine-served
                    // interaction path. Reserve before publishing or waiting
                    // so they share the same hard review-turn allowance.
                    self.automated_review_tool_budgets.reserve(&thread.id)?;
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    persist_deadline = None;
                    let answers = self
                        .ask_user_questions(&thread.id, turn, &request_id, title, questions, cancel)
                        .await?;
                    let _ = responder.send(answers);
                }
                BackendEvent::Completed { usage } => {
                    backend_completed = true;
                    attempt_usage.input_tokens += usage.input_tokens;
                    attempt_usage.output_tokens += usage.output_tokens;
                    attempt_usage.cached_input_tokens += usage.cached_input_tokens;
                    if let Some(cost) = usage.cost_usd {
                        attempt_usage.cost_usd = Some(attempt_usage.cost_usd.unwrap_or(0.0) + cost);
                    }
                    if usage.context_window.is_some() {
                        attempt_usage.context_window = usage.context_window;
                    }
                    if usage.context_input_tokens.is_some() {
                        attempt_usage.context_input_tokens = usage.context_input_tokens;
                    }
                }
            }
            let collaborator_pending = collaborators
                .values()
                .any(|collaborator| !collaborator.persisted.is_empty());
            if persisted.len()
                + collaborators
                    .values()
                    .map(|collaborator| collaborator.persisted.len())
                    .sum::<usize>()
                >= STREAM_EVENT_BATCH_MAX
            {
                flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                persist_deadline = None;
            } else if (!persisted.is_empty() || collaborator_pending) && persist_deadline.is_none()
            {
                persist_deadline = Some(Instant::now() + STREAM_EVENT_BATCH_WINDOW);
            }
          }
          Ok(())
        }
        .await;
        if let Err(error) = event_loop_result {
            attempt_error = Some(error);
        }
        let abort_backend = attempt_error.is_some()
            || backend_error.is_some()
            || cancel.is_cancelled()
            || backend_cancelled;
        // Resolve outstanding approval futures first so a vendor cannot stay
        // blocked on an engine responder while its attempt is being stopped.
        // Already-granted mutation permits remain held in their maps.
        deny_pending_backend_approvals(
            &mut pending_backend_approvals,
            &mut backend_approval_cancels,
            &mut collaborators,
        )
        .await;
        if abort_backend {
            let mut mutation_permits = backend_mutation_permits
                .drain()
                .map(|(_, permit)| permit)
                .collect::<Vec<_>>();
            for collaborator in collaborators.values_mut() {
                mutation_permits.extend(
                    collaborator
                        .mutation_permits
                        .drain()
                        .map(|(_, permit)| permit),
                );
            }
            if drain_or_quarantine_backend(
                stream,
                attempt_cancel,
                self.tool_mutation_admission_lock(&session.id),
                mutation_permits,
                backend_id.clone(),
                thread.id.clone(),
            )
            .await
            {
                let trigger = attempt_error
                    .as_ref()
                    .map(|error| format!("event processing failure: {error:#}"))
                    .or_else(|| {
                        backend_error
                            .as_ref()
                            .map(|error| format!("backend failure: {error}"))
                    })
                    .unwrap_or_else(|| "turn cancellation".into());
                attempt_error = Some(anyhow!(
                    "backend {backend_id} cleanup did not finish within {} ms after {trigger}; \
                     session mutations are quarantined until cleanup completes",
                    BACKEND_CANCEL_CLEANUP_TIMEOUT.as_millis()
                ));
            }
        } else {
            drop(stream);
            backend_mutation_permits.clear();
        }
        flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
        for collaborator in collaborators.values_mut() {
            if !collaborator.terminal {
                let reason = unfinished_collaborator_reason(
                    cancel.is_cancelled() || backend_cancelled,
                    attempt_error.as_ref(),
                    backend_error.as_ref(),
                );
                self.finish_backend_collaborator(session, backend_id, collaborator, Err(reason))
                    .await?;
            }
            collaborator_claims.release(&collaborator.thread.id);
        }
        accounting.add_backend(&attempt_usage);

        if attempt_error.is_some() || backend_error.is_some() {
            if !segment.is_empty() {
                persisted.push(Event::AssistantMessage {
                    turn,
                    content: std::mem::take(&mut segment),
                });
            }
            if !text.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text.clone(),
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            for call_id in open_tools.drain() {
                persisted.push(Event::ToolCompleted {
                    call_id,
                    status: ToolStatus::Aborted,
                    result: serde_json::json!({
                        "error": "provider route ended during tool execution"
                    }),
                    execution_duration_ms: None,
                });
            }
            flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
            if tools_enabled {
                let seen = self.store.messages(&thread.id)?.len() as u64;
                self.store.mark_backend_seen(&thread.id, backend_id, seen)?;
            }
        }

        if let Some(error) = attempt_error {
            self.record_routed_usage(&session.id, &thread.id, turn, accounting, false)?;
            return Err(error);
        }

        if let Some(error) = backend_error {
            return Ok(RouteAttemptResult::Failed(backend_attempt_failure(
                error,
                side_effect_started,
            )));
        }

        if cancel.is_cancelled() || backend_cancelled {
            if !segment.is_empty() {
                persisted.push(Event::AssistantMessage {
                    turn,
                    content: segment,
                });
            }
            for call_id in open_tools {
                persisted.push(Event::ToolCompleted {
                    call_id,
                    status: ToolStatus::Aborted,
                    result: serde_json::json!({
                        "error": "turn cancelled during tool execution"
                    }),
                    execution_duration_ms: None,
                });
            }
            flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
            if !text.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            return Ok(RouteAttemptResult::Cancelled);
        }

        if !segment.is_empty() {
            persisted.push(Event::AssistantMessage {
                turn,
                content: segment,
            });
        }
        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::Assistant {
                content: text,
                tool_calls: Vec::new(),
                reasoning: Vec::new(),
            })?,
        )?;
        if tools_enabled {
            let seen = self.store.messages(&thread.id)?.len() as u64;
            self.store.mark_backend_seen(&thread.id, backend_id, seen)?;
        }
        Ok(RouteAttemptResult::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct CatalogTestProvider;

    struct FailingCatalogProvider {
        id: String,
        calls: Arc<AtomicUsize>,
    }

    struct BlockingFailingPinnedProvider {
        started: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
    }

    struct DiscoveringCatalogProvider {
        id: String,
        calls: Arc<AtomicUsize>,
    }

    struct AliasedCatalogProvider {
        calls: Arc<AtomicUsize>,
    }

    struct StallingCatalogProvider {
        id: String,
        shared_model: String,
        calls: Arc<AtomicUsize>,
    }

    #[derive(Debug)]
    struct RecordedBackendTurn {
        model: String,
        prompt: String,
        read_only: bool,
        full_bridge: bool,
        mcp_server_count: usize,
        attach_background: bool,
    }

    struct RecordingBackend {
        turns: Arc<Mutex<Vec<RecordedBackendTurn>>>,
    }

    struct CancellableStartupBackend {
        started: Arc<tokio::sync::Semaphore>,
    }

    fn catalog_model(id: impl Into<String>) -> trouve_protocol::ModelInfo {
        let id = id.into();
        trouve_protocol::ModelInfo {
            display_name: id.clone(),
            id,
            context_window: 100_000,
            supports_tools: true,
            supports_images: false,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    #[async_trait::async_trait]
    impl trouve_providers::Provider for CatalogTestProvider {
        fn id(&self) -> &str {
            "test"
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[trouve_providers::Message],
            _tools: &[trouve_providers::ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> std::result::Result<trouve_providers::EventStream, trouve_providers::ProviderError>
        {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[async_trait::async_trait]
    impl trouve_providers::Provider for FailingCatalogProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn shared_model_identity(&self, model: &str) -> Option<String> {
            (model == "shared").then(|| model.to_string())
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_model(format!("{}/shared", self.id))]
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[trouve_providers::Message],
            _tools: &[trouve_providers::ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> std::result::Result<trouve_providers::EventStream, trouve_providers::ProviderError>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(trouve_providers::ProviderError::Request(
                "injected route outage".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl trouve_providers::Provider for BlockingFailingPinnedProvider {
        fn id(&self) -> &str {
            "provider"
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_model("provider/model")]
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[trouve_providers::Message],
            _tools: &[trouve_providers::ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> std::result::Result<trouve_providers::EventStream, trouve_providers::ProviderError>
        {
            self.started.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            Err(trouve_providers::ProviderError::Api(
                "HTTP 429 Too Many Requests".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl trouve_providers::Provider for DiscoveringCatalogProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn shared_model_identity(&self, model: &str) -> Option<String> {
            matches!(model, "static" | "live").then(|| model.to_string())
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_model(format!("{}/static", self.id))]
        }

        async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            vec![catalog_model(format!("{}/live", self.id))]
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[trouve_providers::Message],
            _tools: &[trouve_providers::ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> std::result::Result<trouve_providers::EventStream, trouve_providers::ProviderError>
        {
            unreachable!("catalog discovery test does not run a turn")
        }
    }

    #[async_trait::async_trait]
    impl trouve_providers::Provider for AliasedCatalogProvider {
        fn id(&self) -> &str {
            "alias"
        }

        fn shared_model_identity(&self, model: &str) -> Option<String> {
            (model == "provider-alias").then(|| "canonical-model".to_string())
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_model("alias/provider-alias")]
        }

        async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.models()
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[trouve_providers::Message],
            _tools: &[trouve_providers::ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> std::result::Result<trouve_providers::EventStream, trouve_providers::ProviderError>
        {
            unreachable!("catalog alias test does not run a turn")
        }
    }

    #[async_trait::async_trait]
    impl trouve_providers::Provider for StallingCatalogProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn shared_model_identity(&self, model: &str) -> Option<String> {
            (model == self.shared_model).then(|| model.to_string())
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_model(format!("{}/{}", self.id, self.shared_model))]
        }

        async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            futures::future::pending().await
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[trouve_providers::Message],
            _tools: &[trouve_providers::ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> std::result::Result<trouve_providers::EventStream, trouve_providers::ProviderError>
        {
            unreachable!("catalog discovery test does not run a turn")
        }
    }

    #[async_trait::async_trait]
    impl AgentBackend for RecordingBackend {
        fn id(&self) -> &str {
            "backend"
        }

        fn shared_model_identity(&self, model: &str) -> Option<String> {
            (model == "shared").then(|| model.to_string())
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_model("backend/shared")]
        }

        fn status(&self) -> trouve_agents::BackendStatus {
            trouve_agents::BackendStatus {
                installed: true,
                has_credentials: true,
            }
        }

        async fn start_login(
            &self,
        ) -> std::result::Result<trouve_agents::BackendLogin, BackendError> {
            unreachable!("routing test does not start a login")
        }

        async fn run_turn(
            &self,
            turn: BackendTurn,
        ) -> std::result::Result<trouve_agents::BackendEventStream, BackendError> {
            self.turns.lock().unwrap().push(RecordedBackendTurn {
                model: turn.model,
                prompt: turn.prompt,
                read_only: matches!(turn.permission, BackendPermission::ReadOnly),
                full_bridge: turn
                    .mcp_bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.bridge_tools),
                mcp_server_count: turn.mcp_servers.len(),
                attach_background: turn.attach_background,
            });
            Ok(Box::pin(futures::stream::iter([Ok(
                BackendEvent::Completed {
                    usage: Usage::default(),
                },
            )])))
        }
    }

    #[async_trait::async_trait]
    impl AgentBackend for CancellableStartupBackend {
        fn id(&self) -> &str {
            "cancellable-startup"
        }

        fn shared_model_identity(&self, model: &str) -> Option<String> {
            (model == "shared").then(|| model.to_string())
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![catalog_model("cancellable-startup/shared")]
        }

        fn status(&self) -> trouve_agents::BackendStatus {
            trouve_agents::BackendStatus {
                installed: true,
                has_credentials: true,
            }
        }

        async fn start_login(
            &self,
        ) -> std::result::Result<trouve_agents::BackendLogin, BackendError> {
            unreachable!("routing test does not start a login")
        }

        async fn run_turn(
            &self,
            turn: BackendTurn,
        ) -> std::result::Result<trouve_agents::BackendEventStream, BackendError> {
            self.started.add_permits(1);
            turn.cancel.cancelled().await;
            Err(BackendError::Cancelled)
        }
    }

    fn model_thread(store: &Store, path: &Path, suffix: &str, model: &str) -> Thread {
        let workspace = Workspace {
            id: format!("ws_{suffix}"),
            name: format!("routing {suffix}"),
            path: path.to_string_lossy().into_owned(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: format!("se_{suffix}"),
            workspace_id: workspace.id.clone(),
            title: format!("Routing {suffix}"),
            branch: "main".into(),
            worktree_path: workspace.path,
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: format!("th_{suffix}"),
            session_id: session.id,
            parent_thread_id: None,
            title: None,
            mode: "plan".into(),
            model: model.into(),
            model_options: Default::default(),
            permission_mode: trouve_protocol::PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store.insert_thread(&thread, &Default::default()).unwrap();
        thread
    }

    fn routing_thread(store: &Store, path: &Path, suffix: &str) -> Thread {
        model_thread(store, path, suffix, "auto/shared")
    }

    async fn wait_for_terminal_turn(engine: &Engine, store: &Store, thread: &Thread, turn: u64) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let terminal = store
                    .events_after(&Scope::Thread(thread.id.clone()), 0)
                    .unwrap()
                    .iter()
                    .any(|event| {
                        matches!(
                            event.event,
                            Event::TurnCompleted { turn: event_turn, .. }
                                | Event::TurnFailed {
                                    turn: event_turn,
                                    ..
                                }
                                | Event::TurnCancelled { turn: event_turn }
                                if event_turn == turn
                        )
                    });
                let inactive = !engine
                    .active_threads
                    .lock()
                    .unwrap()
                    .contains_key(&thread.id);
                if terminal && inactive {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("routed turn did not settle");
    }

    fn model_candidate(
        provider_id: &str,
        provider_model: &str,
        shared_model_id: Option<&str>,
    ) -> ModelCandidate {
        ModelCandidate {
            provider_id: provider_id.into(),
            provider_model: provider_model.into(),
            provider_generation: 0,
            info: fallback_model_info(&format!("{provider_id}/{provider_model}"), provider_model),
            executor: ModelExecutor::Native(Arc::new(CatalogTestProvider)),
            shared_model_id: shared_model_id.map(String::from),
        }
    }

    #[test]
    fn model_selectors_reject_empty_or_whitespace_segments() {
        for invalid in [
            "",
            " gpt-5.6-sol",
            "gpt-5.6-sol ",
            "/gpt-5.6-sol",
            "openai//gpt-5.6-sol",
            "openai/ gpt-5.6-sol",
            "openai/gpt-5.6-sol ",
            "openai/gpt-5.6-sol/",
            "default",
            "AUTO",
        ] {
            assert_eq!(neutral_model_id(invalid), None, "{invalid:?}");
        }
        assert_eq!(neutral_model_id("gpt-5.6-sol"), Some("gpt-5.6-sol"));
        assert_eq!(
            neutral_model_id("anthropic/claude-sonnet-4.5"),
            Some("anthropic/claude-sonnet-4.5")
        );

        for invalid in [
            "openai/",
            "/gpt-5.6-sol",
            " openai/gpt-5.6-sol",
            "openai /gpt-5.6-sol",
            "openai/ gpt-5.6-sol",
            "openai/gpt-5.6-sol ",
            "openai/gpt//variant",
        ] {
            assert_eq!(valid_concrete_selection(invalid), None, "{invalid:?}");
        }
        assert_eq!(
            valid_concrete_selection("openai/gpt-5.6-sol"),
            Some(("openai", "gpt-5.6-sol"))
        );
    }

    #[test]
    fn picker_catalog_keeps_automatic_and_concrete_choices() {
        let catalog = routed_model_catalog(vec![
            model_candidate("openai", "gpt-5.6-sol", Some("gpt-5.6-sol")),
            model_candidate("codex", "gpt-5.6-sol", Some("gpt-5.6-sol")),
            model_candidate("local", "gpt-5.6-sol", None),
        ]);
        let ids = catalog
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                "auto/gpt-5.6-sol",
                "codex/gpt-5.6-sol",
                "local/gpt-5.6-sol",
                "openai/gpt-5.6-sol",
            ]
        );
        let automatic = catalog
            .iter()
            .find(|model| model.id == "auto/gpt-5.6-sol")
            .unwrap();
        assert_eq!(automatic.routes.len(), 2);
        assert!(
            automatic
                .routes
                .iter()
                .all(|route| route.provider_id != "local")
        );
    }

    #[test]
    fn one_route_automatic_schema_still_uses_portable_thinking() {
        let mut candidate = model_candidate("codex", "gpt-5.6-sol", Some("gpt-5.6-sol"));
        candidate.info.options_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "reasoning_effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high"],
                    "default": "high"
                },
                "fast": {"type": "boolean", "default": false}
            }
        });

        let automatic = routed_model_info("auto/gpt-5.6-sol".into(), vec![candidate.clone()]);
        assert_eq!(
            automatic
                .options_schema
                .pointer("/properties/thinking_level/default"),
            Some(&serde_json::json!("high"))
        );
        assert!(
            automatic
                .options_schema
                .pointer("/properties/reasoning_effort")
                .is_none()
        );
        assert!(
            automatic
                .options_schema
                .pointer("/properties/fast")
                .is_some()
        );

        let concrete = routed_model_info("codex/gpt-5.6-sol".into(), vec![candidate.clone()]);
        assert_eq!(concrete.options_schema, candidate.info.options_schema);
    }

    #[test]
    fn multi_route_schema_and_options_preserve_only_portable_settings() {
        let model = |provider_id: &str, thinking_key: &str, default: &str, provider_only: bool| {
            let mut candidate = model_candidate(provider_id, "shared", Some("shared"));
            let mut properties = serde_json::Map::from_iter([
                (
                    thinking_key.to_string(),
                    serde_json::json!({
                        "type": "string",
                        "enum": ["low", "medium", "high"],
                        "default": default
                    }),
                ),
                (
                    "fast".into(),
                    serde_json::json!({"type": "boolean", "default": false}),
                ),
            ]);
            if provider_only {
                properties.insert(
                    "provider_only".into(),
                    serde_json::json!({"type": "string"}),
                );
            }
            candidate.info.options_schema = serde_json::json!({
                "type": "object",
                "properties": properties
            });
            candidate
        };
        let codex = model("codex", "reasoning_effort", "high", true);
        let cursor = model("cursor", "thinking_level", "low", false);
        let automatic =
            routed_model_info("auto/shared".into(), vec![codex.clone(), cursor.clone()]);

        assert_eq!(
            automatic
                .options_schema
                .pointer("/properties/thinking_level/default"),
            Some(&serde_json::json!("medium"))
        );
        assert!(
            automatic
                .options_schema
                .pointer("/properties/fast")
                .is_some()
        );
        assert!(
            automatic
                .options_schema
                .pointer("/properties/reasoning_effort")
                .is_none()
        );
        assert!(
            automatic
                .options_schema
                .pointer("/properties/provider_only")
                .is_none()
        );

        let stored = serde_json::Map::from_iter([
            ("thinking_level".into(), serde_json::json!("high")),
            ("fast".into(), serde_json::json!(true)),
            ("provider_only".into(), serde_json::json!("stale")),
        ]);
        let shared = model_options_for_schema(&stored, &model_info_for_routed_selection(automatic));
        let codex_options = model_options_for_schema(&shared, &codex.info);
        assert_eq!(codex_options["reasoning_effort"], "high");
        assert_eq!(codex_options["fast"], true);
        assert!(!codex_options.contains_key("provider_only"));
    }

    #[test]
    fn routed_failures_never_replay_after_a_side_effect() {
        let native = native_attempt_failure(
            trouve_providers::ProviderError::Api("HTTP 429 Too Many Requests".into()),
            true,
        );
        assert_eq!(native.kind, RouteFailureKind::Capacity);
        assert!(!native.safe_to_retry);

        let backend = backend_attempt_failure(
            BackendError::Protocol("HTTP 429 Too Many Requests".into()),
            true,
        );
        assert_eq!(backend.kind, RouteFailureKind::Capacity);
        assert!(!backend.safe_to_retry);
    }

    #[test]
    fn routed_accounting_preserves_the_latest_authoritative_context_size() {
        let route = model_candidate("hosted", "shared", Some("shared"));
        let usage = Usage {
            input_tokens: 10,
            cached_input_tokens: 20,
            context_input_tokens: Some(77),
            ..Usage::default()
        };
        let mut native = TurnAccounting::default();
        native.add_native(
            &trouve_providers::models_dev::ModelsDevCatalog::embedded(),
            &route,
            &usage,
        );
        assert_eq!(native.context_input_tokens, 77);
        assert_eq!(native.usage.context_input_tokens, Some(77));

        let mut backend = TurnAccounting::default();
        backend.add_backend(&usage);
        assert_eq!(backend.context_input_tokens, 77);
        assert_eq!(backend.usage.context_input_tokens, Some(77));
    }

    #[test]
    fn subscription_routes_rank_reported_headroom_and_exhaustion() {
        let health = |status: &str, windows: &[i64]| trouve_protocol::SubscriptionHealth {
            provider_id: "test".into(),
            status: status.into(),
            plan: String::new(),
            windows: windows
                .iter()
                .map(|used_percent| trouve_protocol::SubscriptionWindow {
                    label: "window".into(),
                    used_percent: *used_percent,
                    resets: String::new(),
                })
                .collect(),
            credits: String::new(),
            note: String::new(),
        };

        assert_eq!(subscription_health_rank(&health("ok", &[10, 40])), (0, 40));
        assert_eq!(subscription_health_rank(&health("ok", &[])), (1, 0));
        assert_eq!(
            subscription_health_rank(&health("unavailable", &[])),
            (2, 0)
        );
        assert_eq!(subscription_health_rank(&health("ok", &[100])), (3, 100));
    }

    #[tokio::test]
    async fn provider_preference_orders_healthy_routes_before_fallbacks() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config {
                provider_order: vec!["second".into(), "first".into()],
                local_enabled: Some(false),
                ..Default::default()
            },
        );
        let ranked = engine
            .rank_model_candidates(
                "auto/shared",
                vec![
                    model_candidate("first", "shared", Some("shared")),
                    model_candidate("second", "shared", Some("shared")),
                ],
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.provider_id.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
    }

    #[tokio::test]
    async fn configured_loopback_catalog_provider_is_concrete_only() {
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config {
                providers: BTreeMap::from([(
                    "openai".into(),
                    crate::config::ProviderConfig {
                        kind: "openai-compat".into(),
                        base_url: Some("http://localhost.:9/v1".into()),
                        ..Default::default()
                    },
                )]),
                local_enabled: Some(false),
                ..Default::default()
            },
        );

        let models = engine.list_models().await;
        assert!(models.iter().any(|model| model.id.starts_with("openai/")));
        assert!(models.iter().all(|model| !model.id.starts_with("auto/")));
    }

    #[tokio::test]
    async fn static_model_catalog_does_not_invoke_live_discovery() {
        let data = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_provider(
            "catalog-test",
            Arc::new(DiscoveringCatalogProvider {
                id: "catalog-test".into(),
                calls: calls.clone(),
            }),
        );

        let static_models = engine.list_models().await;
        assert!(
            static_models
                .iter()
                .any(|model| model.id == "catalog-test/static")
        );
        assert!(static_models.iter().any(|model| model.id == "auto/static"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let live_models = engine.refresh_models().await;
        assert!(
            live_models
                .iter()
                .any(|model| model.id == "catalog-test/live")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn automatic_turn_resolution_uses_static_routes_without_live_discovery() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut thread = routing_thread(&store, data.path(), "static_resolution");
        thread.model = "auto/static".into();
        let engine = Engine::new(
            store,
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_provider(
            "catalog-test",
            Arc::new(DiscoveringCatalogProvider {
                id: "catalog-test".into(),
                calls: calls.clone(),
            }),
        );

        let candidates = engine.resolve_model_candidates(&thread).await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].provider_id, "catalog-test");
        assert_eq!(candidates[0].provider_model, "static");
        assert_eq!(
            engine
                .known_automatic_model_info("auto/static")
                .map(|model| model.id),
            Some("auto/static".into())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn live_discovery_times_out_per_adapter_and_uses_static_metadata() {
        let data = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_provider(
            "stall",
            Arc::new(StallingCatalogProvider {
                id: "stall".into(),
                shared_model: "stalled".into(),
                calls: calls.clone(),
            }),
        );

        let routes = tokio::time::timeout(Duration::from_secs(1), engine.list_model_routes())
            .await
            .expect("one stalled adapter must not hang route discovery");
        assert!(routes.iter().any(|model| model.id == "stall/stalled"));
        assert!(routes.iter().any(|model| model.id == "auto/stalled"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn automatic_metadata_validation_skips_unrelated_adapters() {
        let data = tempfile::tempdir().unwrap();
        let live_calls = Arc::new(AtomicUsize::new(0));
        let stalled_calls = Arc::new(AtomicUsize::new(0));
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_provider(
            "catalog-test",
            Arc::new(DiscoveringCatalogProvider {
                id: "catalog-test".into(),
                calls: live_calls.clone(),
            }),
        )
        .with_provider(
            "stall",
            Arc::new(StallingCatalogProvider {
                id: "stall".into(),
                shared_model: "unrelated".into(),
                calls: stalled_calls.clone(),
            }),
        );

        let model = engine.resolve_model_info("auto/live").await.unwrap();
        assert_eq!(model.id, "auto/live");
        assert_eq!(live_calls.load(Ordering::SeqCst), 1);
        assert_eq!(stalled_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn automatic_metadata_validation_follows_provider_local_aliases() {
        let data = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_provider(
            "alias",
            Arc::new(AliasedCatalogProvider {
                calls: calls.clone(),
            }),
        );

        let refreshed = engine
            .refresh_model_candidates_for(Some("auto/canonical-model"))
            .await;
        assert!(
            refreshed
                .iter()
                .any(|candidate| candidate.shared_model_id.as_deref() == Some("canonical-model"))
        );
        let model = engine
            .resolve_model_info("auto/canonical-model")
            .await
            .unwrap();
        assert_eq!(model.id, "auto/canonical-model");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn automatic_routes_cross_adapters_then_keep_healthy_affinity() {
        let data = tempfile::tempdir().unwrap();
        let worktree = data.path().join("worktrees/cross-adapter");
        std::fs::create_dir_all(&worktree).unwrap();
        let store = Store::open_in_memory().unwrap();
        let thread = routing_thread(&store, &worktree, "cross_adapter");
        let api_calls = Arc::new(AtomicUsize::new(0));
        let backend_turns = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            providers: BTreeMap::from([(
                "backend".into(),
                crate::config::ProviderConfig {
                    kind: "claude-cli".into(),
                    tool_bridge: Some(true),
                    ..Default::default()
                },
            )]),
            provider_order: vec!["api".into(), "backend".into()],
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Arc::new(
            Engine::new(store.clone(), data.path().into(), &config)
                .with_provider(
                    "api",
                    Arc::new(FailingCatalogProvider {
                        id: "api".into(),
                        calls: api_calls.clone(),
                    }),
                )
                .with_backend(
                    "backend",
                    Arc::new(trouve_agents::RetirementAwareBackend::new(Arc::new(
                        RecordingBackend {
                            turns: backend_turns.clone(),
                        },
                    ))),
                ),
        );
        engine.set_base_url("http://127.0.0.1:4000");

        engine
            .send_message(
                &thread.id,
                "Start the task".into(),
                vec![trouve_protocol::AttachmentUpload {
                    name: "handoff.txt".into(),
                    mime: "text/plain".into(),
                    data: "ZXZpZGVuY2U=".into(),
                }],
            )
            .unwrap();
        wait_for_terminal_turn(&engine, &store, &thread, 1).await;

        assert_eq!(
            api_calls.load(Ordering::SeqCst),
            1,
            "events: {:?}",
            store
                .events_after(&Scope::Thread(thread.id.clone()), 0)
                .unwrap()
        );
        {
            let turns = backend_turns.lock().unwrap();
            assert_eq!(turns.len(), 1);
            assert_eq!(turns[0].model, "shared");
            assert!(
                turns[0]
                    .prompt
                    .contains("Another provider could not continue")
            );
            assert!(
                turns[0].prompt.contains("handoff.txt"),
                "non-image attachments must remain visible after failover"
            );
            assert!(turns[0].read_only);
            assert!(turns[0].full_bridge);
            assert_eq!(turns[0].mcp_server_count, 0);
        }
        assert_eq!(
            store.thread_route_affinity(&thread.id).unwrap(),
            Some(("backend".into(), "shared".into()))
        );

        engine
            .send_message(&thread.id, "Continue the task".into(), Vec::new())
            .unwrap();
        wait_for_terminal_turn(&engine, &store, &thread, 2).await;

        assert_eq!(
            api_calls.load(Ordering::SeqCst),
            1,
            "a healthy affinity should avoid replaying history to the failed API route"
        );
        assert_eq!(backend_turns.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn automatic_background_attach_stays_on_its_signaling_backend() {
        let data = tempfile::tempdir().unwrap();
        let worktree = data.path().join("worktrees/background-attach");
        std::fs::create_dir_all(&worktree).unwrap();
        let store = Store::open_in_memory().unwrap();
        let thread = routing_thread(&store, &worktree, "background_attach");
        store
            .set_thread_route_affinity(&thread.id, &thread.model, "backend", "shared")
            .unwrap();
        let api_calls = Arc::new(AtomicUsize::new(0));
        let backend_turns = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            providers: BTreeMap::from([(
                "backend".into(),
                crate::config::ProviderConfig {
                    kind: "claude-cli".into(),
                    tool_bridge: Some(true),
                    ..Default::default()
                },
            )]),
            provider_order: vec!["api".into(), "backend".into()],
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Arc::new(
            Engine::new(store.clone(), data.path().into(), &config)
                .with_provider(
                    "api",
                    Arc::new(FailingCatalogProvider {
                        id: "api".into(),
                        calls: api_calls.clone(),
                    }),
                )
                .with_backend(
                    "backend",
                    Arc::new(RecordingBackend {
                        turns: backend_turns.clone(),
                    }),
                ),
        );
        engine.set_base_url("http://127.0.0.1:4000");

        engine
            .dispatch_background_attach_turn(&thread.id, "backend")
            .unwrap();
        wait_for_terminal_turn(&engine, &store, &thread, 1).await;

        assert_eq!(api_calls.load(Ordering::SeqCst), 0);
        let turns = backend_turns.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert!(turns[0].attach_background);
        assert_eq!(
            background_attach_backend_id(&turns[0].prompt),
            Some("backend")
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_backend_startup_without_poisoning_route_health() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let thread = routing_thread(&store, data.path(), "cancel_startup");
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let engine = Arc::new(
            Engine::new(
                store.clone(),
                data.path().into(),
                &Config {
                    local_enabled: Some(false),
                    ..Default::default()
                },
            )
            .with_backend(
                "cancellable-startup",
                Arc::new(CancellableStartupBackend {
                    started: started.clone(),
                }),
            ),
        );

        engine
            .send_message(&thread.id, "Wait during startup".into(), Vec::new())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), started.acquire())
            .await
            .expect("backend startup did not begin")
            .expect("startup signal remains open")
            .forget();
        engine.cancel_turn(&thread.id).unwrap();
        wait_for_terminal_turn(&engine, &store, &thread, 1).await;

        assert!(
            store
                .events_after(&Scope::Thread(thread.id.clone()), 0)
                .unwrap()
                .iter()
                .any(|event| matches!(event.event, Event::TurnCancelled { turn: 1 }))
        );
        assert!(
            store.route_health().unwrap().is_empty(),
            "user cancellation is not provider failure"
        );
    }

    #[tokio::test]
    async fn automatic_turns_bound_large_broken_provider_sets() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let thread = routing_thread(&store, data.path(), "bounded_failover");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut engine = Engine::new(
            store.clone(),
            data.path().into(),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        );
        for index in 0..30 {
            let id = format!("provider-{index:02}");
            engine = engine.with_provider(
                &id,
                Arc::new(FailingCatalogProvider {
                    id: id.clone(),
                    calls: calls.clone(),
                }),
            );
        }
        let engine = Arc::new(engine);

        engine
            .send_message(&thread.id, "Try bounded routes".into(), Vec::new())
            .unwrap();
        wait_for_terminal_turn(&engine, &store, &thread, 1).await;
        assert_eq!(calls.load(Ordering::SeqCst), MAX_ROUTE_ATTEMPTS_PER_TURN);
        assert_eq!(
            store.route_health().unwrap().len(),
            MAX_ROUTE_ATTEMPTS_PER_TURN
        );

        engine
            .send_message(&thread.id, "Try the next bounded routes".into(), Vec::new())
            .unwrap();
        wait_for_terminal_turn(&engine, &store, &thread, 2).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2 * MAX_ROUTE_ATTEMPTS_PER_TURN
        );
        assert_eq!(
            store.route_health().unwrap().len(),
            2 * MAX_ROUTE_ATTEMPTS_PER_TURN
        );
    }

    #[test]
    fn stale_scheduler_outcome_cannot_override_a_newer_success() {
        let scheduler = TurnScheduler::new();
        let older = scheduler.next_attempt_order();
        let newer = scheduler.next_attempt_order();
        scheduler.record_ordered_outcome("provider/model", None, newer);
        scheduler.record_ordered_outcome(
            "provider/model",
            Some("HTTP 429 Too Many Requests"),
            older,
        );
        assert_eq!(scheduler.cooldown_remaining("provider/model"), None);
    }

    #[tokio::test]
    async fn older_pinned_admission_cannot_clear_a_newer_routed_cooldown() {
        let scheduler = TurnScheduler::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let pinned = scheduler.admit("provider/model", &cancel).await.unwrap();
        let routed = scheduler.admit("provider/model", &cancel).await.unwrap();
        assert!(pinned.attempt_order < routed.attempt_order);

        scheduler.record_ordered_outcome(
            "provider/model",
            Some("HTTP 429 Too Many Requests"),
            routed.attempt_order,
        );
        scheduler.record_ordered_outcome("provider/model", None, pinned.attempt_order);

        assert!(scheduler.cooldown_remaining("provider/model").is_some());
    }

    #[test]
    fn stale_provider_outcome_cannot_restore_invalidated_route_health() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let engine = Engine::new(store.clone(), data.path().into(), &Config::default())
            .with_provider("provider", Arc::new(CatalogTestProvider));
        let stale_generation = engine
            .provider_registry_snapshot()
            .0
            .into_iter()
            .find(|(id, _, _)| id == "provider")
            .map(|(_, generation, _)| generation)
            .unwrap();
        store
            .record_route_failure("provider", "shared", 1, 10, 30)
            .unwrap();

        engine.invalidate_provider_route_state("provider").unwrap();
        assert!(store.route_health().unwrap().is_empty());

        let applied = engine
            .with_current_provider_generation("provider", stale_generation, || {
                store.record_route_failure("provider", "shared", 2, 10, 30)?;
                Ok(())
            })
            .unwrap();

        assert!(applied.is_none());
        assert!(store.route_health().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_pinned_turn_cannot_restore_replaced_provider_cooldown() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let thread = model_thread(&store, data.path(), "stale-pinned", "provider/model");
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let engine = Arc::new(
            Engine::new(store.clone(), data.path().into(), &Config::default()).with_provider(
                "provider",
                Arc::new(BlockingFailingPinnedProvider {
                    started: started.clone(),
                    release: release.clone(),
                }),
            ),
        );

        engine
            .send_message(&thread.id, "Wait for replacement".into(), Vec::new())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), started.acquire())
            .await
            .expect("pinned provider request did not start")
            .unwrap()
            .forget();

        {
            let _transition = engine
                .provider_transition_lock("provider")
                .lock_owned()
                .await;
            engine.invalidate_provider_route_state("provider").unwrap();
            engine
                .providers
                .write()
                .unwrap()
                .insert("provider".into(), Arc::new(CatalogTestProvider));
        }
        release.add_permits(1);
        wait_for_terminal_turn(&engine, &store, &thread, 1).await;

        assert_eq!(
            engine.turn_scheduler.cooldown_remaining("provider/model"),
            None,
            "the replaced provider's late 429 restored a stale cooldown"
        );
    }

    #[test]
    fn provider_order_is_not_published_when_config_persistence_fails() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let config = Config {
            providers: BTreeMap::from([
                ("first".into(), crate::config::ProviderConfig::default()),
                ("second".into(), crate::config::ProviderConfig::default()),
            ]),
            provider_order: vec!["first".into(), "second".into()],
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Engine::new(store.clone(), data.path().into(), &config)
            // A directory cannot be atomically replaced with serialized TOML.
            .with_config_file(Some(data.path().to_path_buf()));
        let before = store.latest_event_cursor(&Scope::Server).unwrap();

        assert!(
            engine
                .set_provider_order(&["second".into(), "first".into()], None)
                .is_err()
        );
        assert_eq!(
            engine.config.lock().unwrap().provider_order,
            vec!["first", "second"]
        );
        assert!(
            store
                .events_after(&Scope::Server, before)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn provider_order_rejects_a_stale_snapshot() {
        let data = tempfile::tempdir().unwrap();
        let config = Config {
            providers: BTreeMap::from([
                ("first".into(), crate::config::ProviderConfig::default()),
                ("second".into(), crate::config::ProviderConfig::default()),
            ]),
            provider_order: vec!["first".into(), "second".into()],
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &config,
        );
        let observed = engine.list_providers().provider_order;

        engine
            .set_provider_order(&["second".into(), "first".into()], Some(&observed))
            .unwrap();
        let error = engine
            .set_provider_order(&["first".into(), "second".into()], Some(&observed))
            .unwrap_err();

        assert!(matches!(error, EngineError::Conflict(_)));
        assert_eq!(
            engine.config.lock().unwrap().provider_order,
            vec!["second", "first"]
        );
    }

    #[test]
    fn provider_order_snapshot_and_update_share_injected_provider_membership() {
        let data = tempfile::tempdir().unwrap();
        let config = Config {
            providers: BTreeMap::from([(
                "configured".into(),
                crate::config::ProviderConfig::default(),
            )]),
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Engine::new(
            Store::open_in_memory().unwrap(),
            data.path().into(),
            &config,
        )
        .with_provider("injected", Arc::new(CatalogTestProvider));
        let listed = engine.list_providers();
        assert_eq!(
            listed
                .providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["configured", "injected"]
        );
        assert_eq!(listed.provider_order, vec!["configured", "injected"]);

        engine
            .set_provider_order(
                &["injected".into(), "configured".into()],
                Some(&listed.provider_order),
            )
            .unwrap();

        assert_eq!(
            engine.list_providers().provider_order,
            vec!["injected", "configured"]
        );
    }

    #[test]
    fn provider_order_is_published_for_other_clients_and_cold_start() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let config_path = data.path().join("config.toml");
        let config = Config {
            providers: BTreeMap::from([
                ("first".into(), crate::config::ProviderConfig::default()),
                ("second".into(), crate::config::ProviderConfig::default()),
            ]),
            provider_order: vec!["first".into(), "second".into()],
            local_enabled: Some(false),
            ..Default::default()
        };
        config.save_to(&config_path).unwrap();
        let engine = Engine::new(store.clone(), data.path().into(), &config)
            .with_config_file(Some(config_path.clone()));
        let before = store.latest_event_cursor(&Scope::Server).unwrap();

        engine
            .set_provider_order(&["second".into(), "first".into()], None)
            .unwrap();

        let events = store.events_after(&Scope::Server, before).unwrap();
        assert!(matches!(
            events.as_slice(),
            [trouve_protocol::EventEnvelope {
                event: Event::ProviderOrderUpdated { provider_order },
                ..
            }] if provider_order.starts_with(&["second".into(), "first".into()])
        ));
        let (_, projection) = engine.server_projection_snapshot().unwrap();
        assert!(
            projection
                .provider_order
                .starts_with(&["second".into(), "first".into()])
        );
        assert_eq!(
            Config::load_from(&config_path).provider_order,
            vec!["second", "first"]
        );
    }

    #[tokio::test]
    async fn published_provider_update_clears_persistent_and_scheduler_cooldowns() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .record_route_failure("provider", "model", 1, 60, 60)
            .unwrap();
        let config = Config {
            providers: BTreeMap::from([(
                "provider".into(),
                crate::config::ProviderConfig {
                    kind: "openai-compat".into(),
                    base_url: Some("https://old.example.test/v1".into()),
                    api_key: Some("test-key".into()),
                    ..Default::default()
                },
            )]),
            local_enabled: Some(false),
            ..Default::default()
        };
        let engine = Arc::new(Engine::new(store.clone(), data.path().into(), &config));
        let attempt_order = engine.turn_scheduler.next_attempt_order();
        engine.turn_scheduler.record_ordered_outcome(
            "provider/model",
            Some("HTTP 429 Too Many Requests"),
            attempt_order,
        );
        assert!(
            engine
                .turn_scheduler
                .cooldown_remaining("provider/model")
                .is_some()
        );

        engine
            .upsert_provider(
                "provider",
                &UpsertProviderRequest {
                    kind: "openai-compat".into(),
                    base_url: Some("https://new.example.test/v1".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert!(store.route_health().unwrap().is_empty());
        assert_eq!(
            engine.turn_scheduler.cooldown_remaining("provider/model"),
            None
        );
    }

    #[tokio::test]
    async fn route_health_clear_failure_rejects_provider_update_without_publication() {
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("trouve.sqlite3");
        let config_path = data.path().join("config.toml");
        let store = Store::open(&database).unwrap();
        let previous_health = store
            .record_route_failure("provider", "model", 1, 60, 60)
            .unwrap();
        let config = Config {
            providers: BTreeMap::from([(
                "provider".into(),
                crate::config::ProviderConfig {
                    kind: "openai-compat".into(),
                    base_url: Some("https://old.example.test/v1".into()),
                    ..Default::default()
                },
            )]),
            local_enabled: Some(false),
            ..Default::default()
        };
        config.save_to(&config_path).unwrap();
        let engine = Arc::new(
            Engine::new(store.clone(), data.path().into(), &config)
                .with_config_file(Some(config_path.clone())),
        );
        let attempt_order = engine.turn_scheduler.next_attempt_order();
        engine.turn_scheduler.record_ordered_outcome(
            "provider/model",
            Some("HTTP 429 Too Many Requests"),
            attempt_order,
        );
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_route_health_clear
                 BEFORE DELETE ON route_health
                 WHEN OLD.provider_id = 'provider'
                 BEGIN
                   SELECT RAISE(ABORT, 'injected route-health clear failure');
                 END;",
            )
            .unwrap();

        let result = engine
            .upsert_provider(
                "provider",
                &UpsertProviderRequest {
                    kind: "openai-compat".into(),
                    base_url: Some("https://new.example.test/v1".into()),
                    ..Default::default()
                },
            )
            .await;

        assert!(result.is_err());
        assert_eq!(
            engine.config.lock().unwrap().providers["provider"]
                .base_url
                .as_deref(),
            Some("https://old.example.test/v1")
        );
        assert_eq!(
            Config::load_from(&config_path).providers["provider"]
                .base_url
                .as_deref(),
            Some("https://old.example.test/v1")
        );
        assert_eq!(
            store
                .route_health()
                .unwrap()
                .get(&("provider".into(), "model".into())),
            Some(&previous_health)
        );
        assert!(
            engine
                .turn_scheduler
                .cooldown_remaining("provider/model")
                .is_some()
        );
    }

    struct RejectingSecretStore;

    impl trouve_providers::secrets::SecretStore for RejectingSecretStore {
        fn get(&self, _key: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        fn set(&self, _key: &str, _value: &str) -> anyhow::Result<()> {
            anyhow::bail!("injected secret write failure")
        }

        fn delete(&self, _key: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_provider_update_preserves_existing_route_health() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let previous_health = store
            .record_route_failure("provider", "model", 1, 60, 60)
            .unwrap();
        let config = Config {
            providers: BTreeMap::from([(
                "provider".into(),
                crate::config::ProviderConfig {
                    kind: "openai-compat".into(),
                    base_url: Some("https://old.example.test/v1".into()),
                    ..Default::default()
                },
            )]),
            local_enabled: Some(false),
            ..Default::default()
        };
        let mut engine = Engine::new(store.clone(), data.path().into(), &config);
        engine.secrets = Arc::new(RejectingSecretStore);
        let engine = Arc::new(engine);

        let result = engine
            .upsert_provider(
                "provider",
                &UpsertProviderRequest {
                    kind: "openai-compat".into(),
                    base_url: Some("https://new.example.test/v1".into()),
                    api_key: Some("new-key".into()),
                    ..Default::default()
                },
            )
            .await;

        assert!(result.is_err());
        assert_eq!(
            store
                .route_health()
                .unwrap()
                .get(&("provider".into(), "model".into())),
            Some(&previous_health)
        );
        assert_eq!(
            engine.config.lock().unwrap().providers["provider"]
                .base_url
                .as_deref(),
            Some("https://old.example.test/v1")
        );
    }

    #[tokio::test]
    async fn provider_config_persistence_failure_preserves_definition_and_route_health() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let previous_health = store
            .record_route_failure("provider", "model", 1, 60, 60)
            .unwrap();
        let config = Config {
            providers: BTreeMap::from([(
                "provider".into(),
                crate::config::ProviderConfig {
                    kind: "openai-compat".into(),
                    base_url: Some("https://old.example.test/v1".into()),
                    api_key: Some("test-key".into()),
                    ..Default::default()
                },
            )]),
            local_enabled: Some(false),
            ..Default::default()
        };
        // A directory cannot be atomically replaced with serialized TOML.
        let engine = Arc::new(
            Engine::new(store.clone(), data.path().into(), &config)
                .with_config_file(Some(data.path().to_path_buf())),
        );

        assert!(
            engine
                .upsert_provider(
                    "provider",
                    &UpsertProviderRequest {
                        kind: "openai-compat".into(),
                        base_url: Some("https://new.example.test/v1".into()),
                        ..Default::default()
                    },
                )
                .await
                .is_err()
        );
        assert_eq!(
            engine.config.lock().unwrap().providers["provider"]
                .base_url
                .as_deref(),
            Some("https://old.example.test/v1")
        );
        assert_eq!(
            store
                .route_health()
                .unwrap()
                .get(&("provider".into(), "model".into())),
            Some(&previous_health)
        );

        assert!(engine.delete_provider("provider").await.is_err());
        assert!(
            engine
                .config
                .lock()
                .unwrap()
                .providers
                .contains_key("provider")
        );
        assert_eq!(
            store
                .route_health()
                .unwrap()
                .get(&("provider".into(), "model".into())),
            Some(&previous_health)
        );
    }

    #[tokio::test]
    async fn failed_config_mutations_preserve_health_after_vendor_backend_rebuild() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let previous_health = store
            .record_route_failure("provider", "model", 1, 60, 60)
            .unwrap();
        let config = Config {
            providers: BTreeMap::from([(
                "provider".into(),
                crate::config::ProviderConfig {
                    kind: "codex-app-server".into(),
                    base_url: Some("https://old.example.test/v1".into()),
                    ..Default::default()
                },
            )]),
            local_enabled: Some(false),
            ..Default::default()
        };
        // Staging next to this directory succeeds, but publishing over the
        // directory fails after route health has been tentatively cleared.
        let engine = Arc::new(
            Engine::new(store.clone(), data.path().into(), &config)
                .with_config_file(Some(data.path().to_path_buf())),
        );
        assert!(engine.backends.read().unwrap().contains_key("provider"));
        let scheduler_backoff = engine.turn_scheduler.provider("provider/model").backoff;
        let attempt_order = engine.turn_scheduler.next_attempt_order();
        engine.turn_scheduler.record_ordered_outcome(
            "provider/model",
            Some("HTTP 429 Too Many Requests"),
            attempt_order,
        );

        let result = engine
            .upsert_provider(
                "provider",
                &UpsertProviderRequest {
                    kind: "codex-app-server".into(),
                    base_url: Some("https://new.example.test/v1".into()),
                    ..Default::default()
                },
            )
            .await;

        assert!(result.is_err());
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if engine.backends.read().unwrap().contains_key("provider") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("previous vendor backend was not rebuilt");
        assert_eq!(
            store
                .route_health()
                .unwrap()
                .get(&("provider".into(), "model".into())),
            Some(&previous_health)
        );
        assert!(Arc::ptr_eq(
            &scheduler_backoff,
            &engine.turn_scheduler.provider("provider/model").backoff
        ));
        assert_eq!(
            engine.config.lock().unwrap().providers["provider"]
                .base_url
                .as_deref(),
            Some("https://old.example.test/v1")
        );

        assert!(engine.delete_provider("provider").await.is_err());
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if engine.backends.read().unwrap().contains_key("provider") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("previous vendor backend was not rebuilt after failed deletion");
        assert_eq!(
            store
                .route_health()
                .unwrap()
                .get(&("provider".into(), "model".into())),
            Some(&previous_health)
        );
        assert!(Arc::ptr_eq(
            &scheduler_backoff,
            &engine.turn_scheduler.provider("provider/model").backoff
        ));
    }

    #[tokio::test]
    async fn retained_pending_writer_keeps_new_readers_behind_the_fence() {
        let admission = Arc::new(tokio::sync::RwLock::new(()));
        let initial_reader = admission.clone().read_owned().await;
        let quarantine = BackendMutationQuarantine::queue(admission.clone()).await;
        assert!(quarantine._pending_acquisition.is_some());

        let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
        let mut competing_reader = tokio::spawn(async move {
            let mut acquisition = Box::pin(admission.read_owned());
            let mut queued_tx = Some(queued_tx);
            futures::future::poll_fn(|context| {
                let result = std::future::Future::poll(acquisition.as_mut(), context);
                if let Some(queued_tx) = queued_tx.take() {
                    let _ = queued_tx.send(matches!(&result, std::task::Poll::Pending));
                }
                result
            })
            .await
        });
        assert!(queued_rx.await.unwrap(), "competing reader was not queued");

        drop(initial_reader);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut competing_reader)
                .await
                .is_err(),
            "competing reader skipped the retained pending writer"
        );

        drop(quarantine);
        tokio::time::timeout(Duration::from_millis(250), competing_reader)
            .await
            .expect("reader remained blocked after the quarantine was dropped")
            .unwrap();
    }
}
