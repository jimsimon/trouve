//! GitHub App-backed, unattended pull-request reviews.
//!
//! OAuth remains exclusively account-centric. This service authenticates as
//! an installed GitHub App, reconciles webhooks with inexpensive polling,
//! and turns each immutable PR head into a normal trouve review session.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use trouve_protocol::{
    CodeReviewDashboard, CodeReviewMode, CodeReviewRepository, ConfigureGithubAppRequest,
    CreateSessionRequest, CreateThreadRequest, Event, GithubAppStatus, PermissionMode,
    ReviewerOverride, ReviewerProfile, ReviewerPromptMode, Scope,
    UpdateCodeReviewRepositoryRequest, UpsertReviewerProfileRequest,
};

use crate::config::GithubReviewAppConfig;
use crate::engine::{Engine, EngineError};
use crate::store::{CodeReviewJobRecord, CodeReviewManualRequest, NewCodeReviewJob};
use crate::tools::{ReviewDiffFile, ReviewRepositoryDiff, ReviewRepositorySync};

const PRIVATE_KEY_SECRET: &str = "github:review-app:private-key";
const WEBHOOK_SECRET: &str = "github:review-app:webhook-secret";
const RECONCILE_INTERVAL_ENV: &str = "TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS";
const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
const JOB_IDLE_INTERVAL: Duration = Duration::from_secs(5);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const REVIEW_BATCH_MAX_BYTES: usize = 128 * 1024;
const REVIEW_BATCH_MAX_FILES: usize = 24;
const MAX_CANDIDATE_FINDINGS: usize = 200;
const MANUAL_REVIEW_MENTION: &str = "@trouve-ai";

fn parse_code_review_poll_interval(value: &str) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

fn code_review_poll_interval() -> Duration {
    let Ok(value) = std::env::var(RECONCILE_INTERVAL_ENV) else {
        return DEFAULT_RECONCILE_INTERVAL;
    };
    match parse_code_review_poll_interval(&value) {
        Some(interval) => interval,
        _ => {
            tracing::warn!(
                variable = RECONCILE_INTERVAL_ENV,
                value,
                default_seconds = DEFAULT_RECONCILE_INTERVAL.as_secs(),
                "invalid code-review poll interval; using the default"
            );
            DEFAULT_RECONCILE_INTERVAL
        }
    }
}

#[derive(Default)]
pub struct CodeReviewRuntime {
    started: AtomicBool,
    state: Mutex<RuntimeState>,
    installation_tokens: tokio::sync::Mutex<HashMap<u64, CachedToken>>,
    reconcile_lock: tokio::sync::Mutex<()>,
    poll_wake: Notify,
    job_wake: Notify,
    running: Mutex<Option<RunningReview>>,
}

#[derive(Clone)]
struct RunningReview {
    job_id: String,
    cancel: CancellationToken,
}

impl CodeReviewRuntime {
    fn cancel_superseded(&self, job_ids: &[String]) {
        if let Some(running) = self.running.lock().unwrap().clone()
            && job_ids.contains(&running.job_id)
        {
            running.cancel.cancel();
        }
    }
}

#[derive(Default)]
struct RuntimeState {
    installation_count: u64,
    last_poll_at: Option<DateTime<Utc>>,
    last_error: String,
    rate_limit_remaining: Option<u64>,
    rate_limit_reset_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Default)]
struct RateInfo {
    remaining: Option<u64>,
    reset_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct AppJwtClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

#[derive(Deserialize)]
struct AppInfo {
    slug: String,
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
}

#[derive(Deserialize)]
struct InstallationTokenResponse {
    token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct InstallationRepositories {
    repositories: Vec<GithubRepository>,
}

#[derive(Deserialize)]
struct GithubRepository {
    full_name: String,
    private: bool,
}

#[derive(Clone, Deserialize)]
struct GithubPullRequest {
    number: u64,
    title: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    state: String,
    base: GithubPullRef,
    head: GithubPullRef,
    #[serde(default)]
    requested_reviewers: Vec<GithubUser>,
}

#[derive(Clone, Deserialize)]
struct GithubPullRef {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
}

#[derive(Clone, Deserialize)]
struct GithubUser {
    login: String,
}

#[derive(Debug, PartialEq, Eq)]
struct ManualReviewComment {
    repository: String,
    installation_id: u64,
    pull_number: u64,
    trigger_key: String,
}

fn contains_manual_review_command(body: &str) -> bool {
    body.lines().any(|line| {
        let mut words = line.split_whitespace();
        words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case(MANUAL_REVIEW_MENTION))
            && words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("review"))
            && words.next().is_none()
    })
}

fn manual_review_comment(payload: &serde_json::Value) -> Option<ManualReviewComment> {
    if payload["action"].as_str()? != "created"
        || !payload["issue"]["pull_request"].is_object()
        || payload["comment"]["user"]["type"]
            .as_str()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("bot"))
        || !matches!(
            payload["comment"]["author_association"].as_str()?,
            "OWNER" | "MEMBER" | "COLLABORATOR"
        )
        || !contains_manual_review_command(payload["comment"]["body"].as_str()?)
    {
        return None;
    }
    let repository = payload["repository"]["full_name"].as_str()?.to_owned();
    let installation_id = payload["installation"]["id"].as_u64()?;
    let pull_number = payload["issue"]["number"].as_u64()?;
    let comment_id = payload["comment"]["id"].as_u64()?;
    (installation_id > 0 && pull_number > 0 && comment_id > 0).then(|| ManualReviewComment {
        repository,
        installation_id,
        pull_number,
        trigger_key: format!("manual:comment:{comment_id}"),
    })
}

#[derive(Debug, PartialEq, Eq)]
struct RequestedReviewTrigger {
    requested_key: String,
    trigger: &'static str,
    comment_key: Option<String>,
}

fn requested_review_triggers(
    mode: CodeReviewMode,
    draft: bool,
    reviewer_generation: Option<u64>,
    replace_reviewer_request: bool,
    comment_requests: &[CodeReviewManualRequest],
) -> Vec<RequestedReviewTrigger> {
    let mut triggers = Vec::new();
    if let Some(generation) = reviewer_generation {
        triggers.push(RequestedReviewTrigger {
            requested_key: format!("manual:{generation}"),
            trigger: "manual",
            comment_key: None,
        });
    } else if replace_reviewer_request {
        triggers.push(RequestedReviewTrigger {
            requested_key: "manual:revision".into(),
            trigger: "manual",
            comment_key: None,
        });
    }
    triggers.extend(
        comment_requests
            .iter()
            .map(|request| RequestedReviewTrigger {
                requested_key: request.trigger_key.clone(),
                trigger: "manual",
                comment_key: Some(request.trigger_key.clone()),
            }),
    );
    if triggers.is_empty() && !draft && mode == CodeReviewMode::Automatic {
        triggers.push(RequestedReviewTrigger {
            requested_key: "automatic".into(),
            trigger: "automatic",
            comment_key: None,
        });
    }
    triggers
}

fn manual_request_can_satisfy_automatic_review(
    mode: CodeReviewMode,
    draft: bool,
    trigger: &str,
) -> bool {
    trigger == "manual" && mode == CodeReviewMode::Automatic && !draft
}

#[derive(Deserialize)]
struct PublishedReview {
    html_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReviewOutput {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    findings: Vec<ReviewFinding>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReviewFinding {
    path: String,
    line: u64,
    #[serde(default = "default_review_side")]
    side: String,
    #[serde(default)]
    severity: String,
    body: String,
}

#[derive(Debug, Clone)]
struct ReviewBatch {
    paths: Vec<String>,
    diff: String,
}

#[derive(Debug, Clone, Serialize)]
struct CandidateFinding {
    reviewer_id: String,
    reviewer_name: String,
    finding: ReviewFinding,
}

fn default_review_side() -> String {
    "RIGHT".into()
}

struct GithubApi {
    http: reqwest::Client,
    authorization: String,
}

impl GithubApi {
    fn new(authorization: String) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent("trouve-code-review")
                .build()?,
            authorization,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("https://api.github.com{path}"))
            .header("Authorization", &self.authorization)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<(T, RateInfo)> {
        decode_response(self.request(reqwest::Method::GET, path).send().await?).await
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(T, RateInfo)> {
        decode_response(
            self.request(reqwest::Method::POST, path)
                .json(body)
                .send()
                .await?,
        )
        .await
    }
}

async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
) -> Result<(T, RateInfo)> {
    let status = response.status();
    let rate = rate_info(response.headers());
    let body = response.text().await?;
    if !status.is_success() {
        bail!("GitHub API {status}: {}", compact_api_error(&body));
    }
    let value = serde_json::from_str(&body).context("decoding GitHub response")?;
    Ok((value, rate))
}

fn compact_api_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(500).collect())
}

fn rate_info(headers: &reqwest::header::HeaderMap) -> RateInfo {
    let remaining = headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let reset_at = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
    RateInfo {
        remaining,
        reset_at,
    }
}

fn app_jwt(app_id: u64, private_key_pem: &str) -> Result<String> {
    let now = Utc::now().timestamp();
    let claims = AppJwtClaims {
        iat: now - 60,
        exp: now + 9 * 60,
        iss: app_id.to_string(),
    };
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .context("invalid GitHub App RSA private key")?;
    jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key)
        .context("signing GitHub App JWT")
}

fn validate_repository(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let valid = parts.by_ref().take(2).all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    }) && parts.next().is_none()
        && repository.contains('/');
    if !valid {
        bail!("invalid GitHub repository name: {repository}");
    }
    Ok(())
}

fn validate_sha(sha: &str) -> Result<()> {
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid Git commit SHA from GitHub");
    }
    Ok(())
}

impl Engine {
    fn review_app_config(&self) -> Result<(GithubReviewAppConfig, String)> {
        let config = self
            .config
            .lock()
            .unwrap()
            .github_review_app
            .clone()
            .ok_or_else(|| anyhow!("GitHub review App is not configured"))?;
        let key = self
            .secrets
            .get(PRIVATE_KEY_SECRET)?
            .ok_or_else(|| anyhow!("GitHub review App private key is missing"))?;
        Ok((config, key))
    }

    fn app_api(app_id: u64, private_key: &str) -> Result<GithubApi> {
        GithubApi::new(format!("Bearer {}", app_jwt(app_id, private_key)?))
    }

    fn record_review_rate(&self, rate: RateInfo) {
        let mut state = self.code_review.state.lock().unwrap();
        if rate.remaining.is_some() {
            state.rate_limit_remaining = rate.remaining;
        }
        if rate.reset_at.is_some() {
            state.rate_limit_reset_at = rate.reset_at;
        }
    }

    fn emit_code_review_updated(&self, job_id: Option<String>) -> Result<(), EngineError> {
        self.store
            .append_event(Scope::Server, Event::CodeReviewUpdated { job_id })?;
        Ok(())
    }

    pub async fn configure_github_review_app(
        &self,
        request: ConfigureGithubAppRequest,
    ) -> Result<GithubAppStatus, EngineError> {
        if request.app_id == 0 || request.private_key_pem.trim().is_empty() {
            return Err(EngineError::BadRequest(
                "app_id and private_key_pem are required".into(),
            ));
        }
        let api = Self::app_api(request.app_id, &request.private_key_pem)
            .map_err(|error| EngineError::BadRequest(error.to_string()))?;
        let (app, rate): (AppInfo, _) = api.get("/app").await.map_err(|error| {
            EngineError::BadRequest(format!("GitHub App validation failed: {error:#}"))
        })?;
        self.secrets
            .set(PRIVATE_KEY_SECRET, &request.private_key_pem)?;
        if request.webhook_secret.is_empty() {
            self.secrets.delete(WEBHOOK_SECRET)?;
        } else {
            self.secrets.set(WEBHOOK_SECRET, &request.webhook_secret)?;
        }
        let snapshot = {
            let mut config = self.config.lock().unwrap();
            config.github_review_app = Some(GithubReviewAppConfig {
                app_id: request.app_id,
                slug: app.slug,
            });
            config.clone()
        };
        self.persist_config(&snapshot);
        self.code_review.installation_tokens.lock().await.clear();
        self.record_review_rate(rate);
        {
            let mut state = self.code_review.state.lock().unwrap();
            state.installation_count = 0;
            state.last_error.clear();
        }
        self.code_review.poll_wake.notify_one();
        self.emit_code_review_updated(None)?;
        self.github_app_status()
    }

    pub fn github_app_status(&self) -> Result<GithubAppStatus, EngineError> {
        let config = self.config.lock().unwrap().github_review_app.clone();
        let private_key_configured = self.secrets.get(PRIVATE_KEY_SECRET)?.is_some();
        let webhook_configured = self
            .secrets
            .get(WEBHOOK_SECRET)?
            .is_some_and(|secret| !secret.is_empty());
        let state = self.code_review.state.lock().unwrap();
        Ok(GithubAppStatus {
            configured: config.is_some() && private_key_configured,
            app_id: config.as_ref().map(|config| config.app_id),
            slug: config
                .as_ref()
                .map(|config| config.slug.clone())
                .unwrap_or_default(),
            bot_login: config
                .as_ref()
                .map(|config| format!("{}[bot]", config.slug))
                .unwrap_or_default(),
            webhook_configured,
            installation_count: state.installation_count,
            last_poll_at: state.last_poll_at,
            last_error: state.last_error.clone(),
            rate_limit_remaining: state.rate_limit_remaining,
            rate_limit_reset_at: state.rate_limit_reset_at,
        })
    }

    pub fn code_review_dashboard(&self) -> Result<CodeReviewDashboard, EngineError> {
        Ok(CodeReviewDashboard {
            app: self.github_app_status()?,
            reviewers: self.code_review_reviewer_catalog()?,
            repositories: self.store.list_code_review_repositories()?,
            jobs: self.store.list_code_review_jobs(100)?,
        })
    }

    fn code_review_reviewer_catalog(&self) -> Result<Vec<ReviewerProfile>, EngineError> {
        let mut reviewers = crate::reviewers::built_in_reviewers();
        for defaults in self.store.list_built_in_reviewer_defaults()? {
            if let Some(reviewer) = reviewers
                .iter_mut()
                .find(|reviewer| reviewer.id == defaults.id)
            {
                reviewer.model = defaults.model;
                reviewer.default_thinking_level = defaults.default_thinking_level;
            }
        }
        reviewers.extend(self.store.list_custom_reviewer_profiles()?);
        Ok(reviewers)
    }

    fn resolve_code_review_reviewers(
        &self,
        ids: &[String],
    ) -> Result<Vec<ReviewerProfile>, EngineError> {
        let catalog = self.code_review_reviewer_catalog()?;
        let by_id: HashMap<_, _> = catalog
            .into_iter()
            .map(|reviewer| (reviewer.id.clone(), reviewer))
            .collect();
        let mut seen = std::collections::HashSet::new();
        let mut resolved = Vec::with_capacity(ids.len());
        for id in ids {
            if !seen.insert(id) {
                return Err(EngineError::BadRequest(format!(
                    "duplicate reviewer id {id:?}"
                )));
            }
            let reviewer = by_id
                .get(id)
                .cloned()
                .ok_or_else(|| EngineError::BadRequest(format!("unknown reviewer id {id:?}")))?;
            resolved.push(reviewer);
        }
        Ok(resolved)
    }

    fn normalize_reviewer_overrides(
        &self,
        overrides: &[ReviewerOverride],
    ) -> Result<Vec<ReviewerOverride>, EngineError> {
        let catalog = self.code_review_reviewer_catalog()?;
        let known: HashSet<_> = catalog
            .iter()
            .map(|reviewer| reviewer.id.as_str())
            .collect();
        let mut seen = HashSet::new();
        let mut normalized = Vec::new();
        for reviewer_override in overrides {
            if !known.contains(reviewer_override.reviewer_id.as_str()) {
                return Err(EngineError::BadRequest(format!(
                    "unknown reviewer id {:?}",
                    reviewer_override.reviewer_id
                )));
            }
            if !seen.insert(reviewer_override.reviewer_id.as_str()) {
                return Err(EngineError::BadRequest(format!(
                    "duplicate reviewer override {:?}",
                    reviewer_override.reviewer_id
                )));
            }
            let model = reviewer_override
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_string);
            if model.as_deref().is_some_and(|model| !model.contains('/')) {
                return Err(EngineError::BadRequest(format!(
                    "model override for reviewer {:?} must be provider-qualified",
                    reviewer_override.reviewer_id
                )));
            }
            let prompt = reviewer_override.prompt.trim();
            if prompt.len() > 16_000 {
                return Err(EngineError::BadRequest(format!(
                    "prompt override for reviewer {:?} is longer than 16000 bytes",
                    reviewer_override.reviewer_id
                )));
            }
            if reviewer_override.prompt_mode != ReviewerPromptMode::Inherit && prompt.is_empty() {
                return Err(EngineError::BadRequest(format!(
                    "prompt override for reviewer {:?} cannot be empty",
                    reviewer_override.reviewer_id
                )));
            }
            if model.is_none() && reviewer_override.prompt_mode == ReviewerPromptMode::Inherit {
                continue;
            }
            normalized.push(ReviewerOverride {
                reviewer_id: reviewer_override.reviewer_id.clone(),
                model,
                prompt_mode: reviewer_override.prompt_mode,
                prompt: if reviewer_override.prompt_mode == ReviewerPromptMode::Inherit {
                    String::new()
                } else {
                    prompt.to_string()
                },
            });
        }
        Ok(normalized)
    }

    pub fn upsert_reviewer_profile(
        &self,
        request: UpsertReviewerProfileRequest,
    ) -> Result<ReviewerProfile, EngineError> {
        let model = request
            .model
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        if model.as_deref().is_some_and(|model| !model.contains('/')) {
            return Err(EngineError::BadRequest(
                "reviewer model must be provider-qualified".into(),
            ));
        }
        let default_thinking_level = request
            .default_thinking_level
            .map(|level| level.trim().to_string())
            .filter(|level| !level.is_empty());
        let updating = request.id.is_some();
        let id = request
            .id
            .unwrap_or_else(|| format!("custom:{}", crate::new_id("rp")));
        if id.len() > 150 {
            return Err(EngineError::BadRequest("reviewer id is too long".into()));
        }
        let (name, prompt, built_in) = if id.starts_with("custom:") {
            let name = request.name.trim();
            let prompt = request.prompt.trim();
            if name.is_empty() || name.len() > 100 {
                return Err(EngineError::BadRequest(
                    "reviewer name must contain 1 to 100 bytes".into(),
                ));
            }
            if prompt.is_empty() || prompt.len() > 16_000 {
                return Err(EngineError::BadRequest(
                    "reviewer prompt must contain 1 to 16000 bytes".into(),
                ));
            }
            if updating
                && !self
                    .store
                    .list_custom_reviewer_profiles()?
                    .iter()
                    .any(|reviewer| reviewer.id == id)
            {
                return Err(EngineError::NotFound(format!("reviewer profile {id}")));
            }
            (name.to_string(), prompt.to_string(), false)
        } else {
            let reviewer = crate::reviewers::built_in_reviewers()
                .into_iter()
                .find(|reviewer| reviewer.id == id)
                .ok_or_else(|| EngineError::NotFound(format!("reviewer profile {id}")))?;
            (reviewer.name, reviewer.prompt, true)
        };
        let reviewer = ReviewerProfile {
            id,
            name,
            prompt,
            model,
            default_thinking_level,
            built_in,
        };
        self.store.upsert_reviewer_profile(&reviewer)?;
        self.code_review.poll_wake.notify_one();
        self.emit_code_review_updated(None)?;
        Ok(reviewer)
    }

    pub fn delete_reviewer_profile(&self, id: &str) -> Result<(), EngineError> {
        if !id.starts_with("custom:") {
            return Err(EngineError::BadRequest(
                "built-in reviewers cannot be deleted".into(),
            ));
        }
        if !self.store.delete_custom_reviewer_profile(id)? {
            return Err(EngineError::NotFound(format!("reviewer profile {id}")));
        }
        self.code_review.poll_wake.notify_one();
        self.emit_code_review_updated(None)?;
        Ok(())
    }

    pub fn update_code_review_repository(
        &self,
        request: &UpdateCodeReviewRepositoryRequest,
    ) -> Result<CodeReviewRepository, EngineError> {
        validate_repository(&request.repository)
            .map_err(|error| EngineError::BadRequest(error.to_string()))?;
        if request
            .model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty())
        {
            return Err(EngineError::BadRequest("model cannot be empty".into()));
        }
        let existing = self
            .store
            .list_code_review_repositories()?
            .into_iter()
            .find(|repository| repository.repository == request.repository);
        let reviewer_ids = request
            .reviewer_ids
            .clone()
            .or_else(|| {
                existing
                    .as_ref()
                    .map(|repository| repository.reviewer_ids.clone())
            })
            .unwrap_or_else(crate::reviewers::default_reviewer_ids);
        if request.mode != CodeReviewMode::Off && reviewer_ids.is_empty() {
            return Err(EngineError::BadRequest(
                "an enabled repository must select at least one reviewer".into(),
            ));
        }
        self.resolve_code_review_reviewers(&reviewer_ids)?;
        let reviewer_overrides = request
            .reviewer_overrides
            .as_ref()
            .cloned()
            .or_else(|| {
                existing
                    .as_ref()
                    .map(|repository| repository.reviewer_overrides.clone())
            })
            .unwrap_or_default();
        let reviewer_overrides = self.normalize_reviewer_overrides(&reviewer_overrides)?;
        let normalized = UpdateCodeReviewRepositoryRequest {
            installation_id: request.installation_id,
            repository: request.repository.clone(),
            mode: request.mode,
            model: request.model.clone(),
            prompt: request.prompt.clone(),
            reviewer_ids: Some(reviewer_ids),
            reviewer_overrides: Some(reviewer_overrides),
        };
        self.store.update_code_review_repository(&normalized)?;
        let repository = self
            .store
            .list_code_review_repositories()?
            .into_iter()
            .find(|repository| repository.repository == request.repository)
            .ok_or_else(|| EngineError::Internal(anyhow!("updated repository disappeared")))?;
        self.code_review.poll_wake.notify_one();
        self.emit_code_review_updated(None)?;
        Ok(repository)
    }

    async fn installation_token(&self, installation_id: u64) -> Result<String> {
        {
            let tokens = self.code_review.installation_tokens.lock().await;
            if let Some(cached) = tokens.get(&installation_id)
                && cached.expires_at > Utc::now() + chrono::Duration::minutes(5)
            {
                return Ok(cached.token.clone());
            }
        }
        let (config, private_key) = self.review_app_config()?;
        let api = Self::app_api(config.app_id, &private_key)?;
        let (created, rate): (InstallationTokenResponse, _) = api
            .post(
                &format!("/app/installations/{installation_id}/access_tokens"),
                &serde_json::json!({}),
            )
            .await?;
        self.record_review_rate(rate);
        self.code_review.installation_tokens.lock().await.insert(
            installation_id,
            CachedToken {
                token: created.token.clone(),
                expires_at: created.expires_at,
            },
        );
        Ok(created.token)
    }

    async fn installation_api(&self, installation_id: u64) -> Result<GithubApi> {
        GithubApi::new(format!(
            "Bearer {}",
            self.installation_token(installation_id).await?
        ))
    }

    pub async fn refresh_code_reviews(&self) -> Result<(), EngineError> {
        self.reconcile_code_reviews()
            .await
            .map_err(|error| EngineError::BadRequest(error.to_string()))
    }

    async fn reconcile_code_reviews(&self) -> Result<()> {
        let _guard = self.code_review.reconcile_lock.lock().await;
        let (config, private_key) = match self.review_app_config() {
            Ok(config) => config,
            Err(_) => return Ok(()),
        };
        let api = Self::app_api(config.app_id, &private_key)?;
        let mut had_errors = false;
        let mut installations = Vec::new();
        let mut installations_complete = true;
        let mut installation_page = 1;
        loop {
            let response = api
                .get(&format!(
                    "/app/installations?per_page=100&page={installation_page}"
                ))
                .await
                .context("listing GitHub App installations");
            let (page, rate): (Vec<Installation>, _) = match response {
                Ok(response) => response,
                Err(error) => {
                    had_errors = true;
                    installations_complete = false;
                    self.record_review_error(format!(
                        "listing GitHub App installations failed: {error:#}"
                    ));
                    break;
                }
            };
            self.record_review_rate(rate);
            let count = page.len();
            installations.extend(page);
            if count < 100 {
                break;
            }
            installation_page += 1;
        }
        if installations_complete {
            let mut state = self.code_review.state.lock().unwrap();
            state.installation_count = installations.len() as u64;
        }

        let mut active_repositories = HashSet::new();
        for installation in installations {
            let installation_api = match self.installation_api(installation.id).await {
                Ok(api) => api,
                Err(error) => {
                    had_errors = true;
                    self.record_review_error(format!(
                        "authenticating GitHub App installation {} failed: {error:#}",
                        installation.id
                    ));
                    continue;
                }
            };
            let mut page = 1;
            loop {
                let response = installation_api
                    .get(&format!(
                        "/installation/repositories?per_page=100&page={page}"
                    ))
                    .await
                    .context("listing installation repositories");
                let (repositories, rate): (InstallationRepositories, _) = match response {
                    Ok(response) => response,
                    Err(error) => {
                        had_errors = true;
                        self.record_review_error(format!(
                            "listing repositories for GitHub App installation {} failed: {error:#}",
                            installation.id
                        ));
                        break;
                    }
                };
                self.record_review_rate(rate);
                let count = repositories.repositories.len();
                for repository in repositories.repositories {
                    active_repositories.insert((installation.id, repository.full_name.clone()));
                    if let Err(error) = self.store.upsert_discovered_code_review_repository(
                        installation.id,
                        &repository.full_name,
                        repository.private,
                    ) {
                        had_errors = true;
                        self.record_review_error(format!(
                            "recording repository {} for GitHub App installation {} failed: {error:#}",
                            repository.full_name, installation.id
                        ));
                    }
                }
                if count < 100 {
                    break;
                }
                page += 1;
            }
        }

        let repositories = self.store.list_code_review_repositories()?;
        for repository in repositories.iter().filter(|repository| {
            repository.mode != CodeReviewMode::Off
                && active_repositories
                    .contains(&(repository.installation_id, repository.repository.clone()))
        }) {
            match self.poll_code_review_repository(repository).await {
                Ok(repository_had_errors) => had_errors |= repository_had_errors,
                Err(error) => {
                    had_errors = true;
                    self.record_review_error(format!(
                        "polling code review repository {} failed: {error:#}",
                        repository.repository
                    ));
                }
            }
        }
        {
            let mut state = self.code_review.state.lock().unwrap();
            state.last_poll_at = Some(Utc::now());
            if !had_errors {
                state.last_error.clear();
            }
        }
        self.emit_code_review_updated(None)?;
        Ok(())
    }

    async fn poll_code_review_repository(&self, repository: &CodeReviewRepository) -> Result<bool> {
        validate_repository(&repository.repository)?;
        let mut comment_requests: HashMap<u64, Vec<CodeReviewManualRequest>> = HashMap::new();
        for request in self
            .store
            .pending_code_review_manual_requests(&repository.repository)?
        {
            comment_requests
                .entry(request.pull_number)
                .or_default()
                .push(request);
        }
        let reviewers = apply_reviewer_overrides(
            self.resolve_code_review_reviewers(&repository.reviewer_ids)?,
            &repository.reviewer_overrides,
        );
        let reviewer_config = serde_json::to_string(&reviewers)?;
        let config_hash = hex::encode(Sha256::digest(
            format!(
                "{:?}\0{}\0{reviewer_config}",
                repository.model, repository.prompt
            )
            .as_bytes(),
        ));
        let api = self.installation_api(repository.installation_id).await?;
        let bot_login = self.github_app_status()?.bot_login;
        let mut pulls = Vec::new();
        let mut page = 1;
        loop {
            let (pull_page, rate): (Vec<GithubPullRequest>, _) = api
                .get(&format!(
                    "/repos/{}/pulls?state=open&per_page=100&page={page}",
                    repository.repository
                ))
                .await
                .with_context(|| format!("listing pull requests for {}", repository.repository))?;
            self.record_review_rate(rate);
            let count = pull_page.len();
            pulls.extend(pull_page);
            if count < 100 {
                break;
            }
            page += 1;
        }
        let mut had_errors = false;
        for pull in pulls {
            let pull_number = pull.number;
            let pending_comments = comment_requests.remove(&pull.number).unwrap_or_default();
            let result = (|| -> Result<()> {
                validate_sha(&pull.base.sha)?;
                validate_sha(&pull.head.sha)?;
                let superseded = self.store.supersede_code_review_jobs(
                    &repository.repository,
                    pull.number,
                    &pull.base.sha,
                    &pull.head.sha,
                    &config_hash,
                )?;
                let review_superseded = !superseded.is_empty();
                if review_superseded {
                    self.code_review.cancel_superseded(&superseded);
                    for job_id in superseded {
                        self.emit_code_review_updated(Some(job_id))?;
                    }
                }
                let manual_requested = pull
                    .requested_reviewers
                    .iter()
                    .any(|reviewer| reviewer.login.eq_ignore_ascii_case(&bot_login));
                let generation = self.store.code_review_manual_transition(
                    &repository.repository,
                    pull.number,
                    manual_requested,
                )?;
                // If a manually requested review is superseded while the bot is
                // still selected, replace it for the new revision/configuration
                // without requiring the user to toggle the request off and on.
                let replace_manual_review = should_replace_manual_review(
                    repository.mode,
                    review_superseded,
                    manual_requested,
                    generation,
                );
                let automatic_key = format!(
                    "{}#{}:{}:{}:automatic:{config_hash}",
                    repository.repository, pull.number, pull.base.sha, pull.head.sha
                );
                let triggers = requested_review_triggers(
                    repository.mode,
                    pull.draft,
                    generation,
                    replace_manual_review,
                    &pending_comments,
                );
                if triggers.is_empty() {
                    return Ok(());
                }

                for requested in triggers {
                    // The first manual request for an unseen automatic head
                    // satisfies its automatic review. Later requests retain
                    // their own stable keys and intentionally run again.
                    let trigger_key = if manual_request_can_satisfy_automatic_review(
                        repository.mode,
                        pull.draft,
                        requested.trigger,
                    ) && !self.store.code_review_job_exists(&automatic_key)?
                    {
                        "automatic".into()
                    } else {
                        requested.requested_key
                    };
                    let dedupe_key = format!(
                        "{}#{}:{}:{}:{trigger_key}:{config_hash}",
                        repository.repository, pull.number, pull.base.sha, pull.head.sha
                    );
                    let job = self.store.enqueue_code_review_job(&NewCodeReviewJob {
                        dedupe_key,
                        installation_id: repository.installation_id,
                        repository: repository.repository.clone(),
                        pull_number: pull.number,
                        pull_title: pull.title.clone(),
                        pull_url: pull.html_url.clone(),
                        head_sha: pull.head.sha.clone(),
                        base_ref: pull.base.sha.clone(),
                        head_ref: pull.head.name.clone(),
                        trigger: requested.trigger.into(),
                        model: repository.model.clone(),
                        prompt: repository.prompt.clone(),
                        reviewers: reviewers.clone(),
                        config_hash: config_hash.clone(),
                    })?;
                    if let Some(comment_key) = requested.comment_key {
                        self.store.complete_code_review_manual_request(
                            &repository.repository,
                            pull.number,
                            &comment_key,
                        )?;
                    }
                    if let Some(job) = job {
                        self.emit_code_review_updated(Some(job.id))?;
                        self.code_review.job_wake.notify_one();
                    }
                }
                Ok(())
            })();

            if let Err(error) = result {
                had_errors = true;
                self.record_review_error(format!(
                    "processing pull request {}#{} failed: {error:#}",
                    repository.repository, pull_number
                ));
            }
        }
        // A request can race with a PR being closed. Once the complete open-PR
        // listing succeeds, unmatched requests have no reviewable target.
        for request in comment_requests.into_values().flatten() {
            self.store.complete_code_review_manual_request(
                &repository.repository,
                request.pull_number,
                &request.trigger_key,
            )?;
        }
        Ok(had_errors)
    }

    pub fn accept_github_review_webhook(
        self: &Arc<Self>,
        event: &str,
        delivery_id: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<(), EngineError> {
        let secret = self
            .secrets
            .get(WEBHOOK_SECRET)?
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| EngineError::BadRequest("GitHub webhooks are not configured".into()))?;
        let signature = signature
            .strip_prefix("sha256=")
            .and_then(|value| hex::decode(value).ok())
            .ok_or_else(|| EngineError::BadRequest("invalid webhook signature".into()))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|error| EngineError::Internal(anyhow!(error)))?;
        mac.update(body);
        mac.verify_slice(&signature)
            .map_err(|_| EngineError::BadRequest("invalid webhook signature".into()))?;
        if !matches!(event, "pull_request" | "issue_comment") {
            self.store
                .claim_github_webhook_delivery(delivery_id, None)?;
            return Ok(());
        }
        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|error| EngineError::BadRequest(format!("invalid webhook JSON: {error}")))?;
        let action = payload["action"].as_str().unwrap_or_default();
        let pull_request_event = event == "pull_request"
            && matches!(
                action,
                "opened"
                    | "reopened"
                    | "synchronize"
                    | "ready_for_review"
                    | "review_requested"
                    | "review_request_removed"
            );
        let manual_comment = (event == "issue_comment")
            .then(|| manual_review_comment(&payload))
            .flatten();
        if !pull_request_event && manual_comment.is_none() {
            self.store
                .claim_github_webhook_delivery(delivery_id, None)?;
            return Ok(());
        }
        let repository_name = manual_comment
            .as_ref()
            .map(|request| request.repository.as_str())
            .or_else(|| payload["repository"]["full_name"].as_str())
            .unwrap_or_default();
        let installation_id = manual_comment
            .as_ref()
            .map(|request| request.installation_id)
            .or_else(|| payload["installation"]["id"].as_u64())
            .unwrap_or_default();
        let repository = self
            .store
            .list_code_review_repositories()?
            .into_iter()
            .find(|repository| {
                repository.repository == repository_name
                    && repository.installation_id == installation_id
                    && repository.mode != CodeReviewMode::Off
            });
        let durable_request = repository.as_ref().and_then(|_| {
            manual_comment.as_ref().map(|request| {
                (
                    request.repository.as_str(),
                    request.pull_number,
                    request.trigger_key.as_str(),
                )
            })
        });
        if !self
            .store
            .claim_github_webhook_delivery(delivery_id, durable_request)?
        {
            return Ok(());
        }
        if let Some(repository) = repository {
            let engine = self.clone();
            tokio::spawn(async move {
                let _guard = engine.code_review.reconcile_lock.lock().await;
                if let Err(error) = engine.poll_code_review_repository(&repository).await {
                    engine.record_review_error(format!("webhook reconciliation failed: {error:#}"));
                }
            });
        } else {
            self.code_review.poll_wake.notify_one();
        }
        Ok(())
    }

    fn record_review_error(&self, error: String) {
        self.code_review.state.lock().unwrap().last_error = error;
        let _ = self.emit_code_review_updated(None);
    }

    pub fn start_code_review_service(self: &Arc<Self>) {
        if self.code_review.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let reconcile_interval = code_review_poll_interval();
        tracing::info!(
            poll_interval_seconds = reconcile_interval.as_secs(),
            "starting GitHub code-review reconciliation"
        );
        if let Err(error) = self.store.recover_code_review_jobs() {
            self.record_review_error(format!("recovering review jobs: {error:#}"));
        }
        let poll_engine = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(error) = poll_engine.reconcile_code_reviews().await {
                    poll_engine
                        .record_review_error(format!("GitHub reconciliation failed: {error:#}"));
                }
                tokio::select! {
                    _ = tokio::time::sleep(reconcile_interval) => {}
                    _ = poll_engine.code_review.poll_wake.notified() => {}
                }
            }
        });
        let worker_engine = self.clone();
        tokio::spawn(async move {
            loop {
                worker_engine.retry_code_review_cleanup().await;
                match worker_engine.store.claim_code_review_job() {
                    Ok(Some(record)) => worker_engine.run_code_review_job(record).await,
                    Ok(None) => {
                        tokio::select! {
                            _ = tokio::time::sleep(JOB_IDLE_INTERVAL) => {}
                            _ = worker_engine.code_review.job_wake.notified() => {}
                        }
                    }
                    Err(error) => {
                        worker_engine
                            .record_review_error(format!("claiming review job: {error:#}"));
                        tokio::time::sleep(JOB_IDLE_INTERVAL).await;
                    }
                }
            }
        });
    }

    async fn run_code_review_job(self: &Arc<Self>, record: CodeReviewJobRecord) {
        let job_id = record.job.id.clone();
        let cancel = CancellationToken::new();
        *self.code_review.running.lock().unwrap() = Some(RunningReview {
            job_id: job_id.clone(),
            cancel: cancel.clone(),
        });
        match self.store.code_review_job(&job_id) {
            Ok(Some(current)) if current.job.status == "running" => {}
            Ok(_) => cancel.cancel(),
            Err(error) => {
                self.record_review_error(format!(
                    "checking whether review job {job_id} is still current: {error:#}"
                ));
            }
        }
        let _ = self.emit_code_review_updated(Some(job_id.clone()));
        let active_thread = Mutex::new(None);
        let result = tokio::time::timeout(
            REVIEW_TIMEOUT,
            self.execute_code_review(&record, &cancel, &active_thread),
        )
        .await;
        if result.is_err() {
            cancel.cancel();
            let active_thread = match active_thread.lock() {
                Ok(mut active_thread) => active_thread.take(),
                Err(error) => {
                    self.record_review_error(format!(
                        "loading active thread for timed-out review job {job_id}: {error}"
                    ));
                    None
                }
            };
            if let Some(thread_id) = active_thread
                && let Err(error) = self.cancel_turn(&thread_id)
            {
                tracing::warn!(
                    job_id,
                    thread_id,
                    %error,
                    "failed to cancel timed-out review thread"
                );
            }
        }
        {
            let mut running = self.code_review.running.lock().unwrap();
            if running
                .as_ref()
                .is_some_and(|running| running.job_id == job_id)
            {
                *running = None;
            }
        }
        let (status, review_url, error) = match result {
            Ok(Ok(url)) => ("succeeded", url, String::new()),
            Ok(Err(error)) if error.to_string().starts_with("stale:") => {
                ("stale", String::new(), error.to_string())
            }
            Ok(Err(error)) => ("failed", String::new(), format!("{error:#}")),
            Err(_) => (
                "failed",
                String::new(),
                format!(
                    "review timed out after {} minutes",
                    REVIEW_TIMEOUT.as_secs() / 60
                ),
            ),
        };
        let finished = if let Err(finish_error) =
            self.store
                .finish_code_review_job(&job_id, status, &review_url, &error)
        {
            self.record_review_error(format!("finishing review job: {finish_error:#}"));
            false
        } else {
            true
        };
        if finished {
            self.retry_code_review_cleanup().await;
        }
        let _ = self.emit_code_review_updated(Some(job_id));
    }

    async fn retry_code_review_cleanup(&self) {
        let pending = match self.store.pending_code_review_job_cleanups() {
            Ok(pending) => pending,
            Err(error) => {
                self.record_review_error(format!(
                    "listing completed review sessions for cleanup: {error:#}"
                ));
                return;
            }
        };
        for (job_id, session_id) in pending {
            match self.delete_session(&session_id).await {
                Ok(()) | Err(EngineError::NotFound(_)) => {
                    if let Err(error) = self
                        .store
                        .complete_code_review_job_cleanup(&job_id, &session_id)
                    {
                        self.record_review_error(format!(
                            "recording cleanup of review job {job_id}: {error:#}"
                        ));
                    }
                }
                Err(error) => {
                    self.record_review_error(format!(
                        "cleaning up terminal review job {job_id}: {error}"
                    ));
                }
            }
        }
    }

    async fn execute_code_review(
        self: &Arc<Self>,
        record: &CodeReviewJobRecord,
        superseded: &CancellationToken,
        active_thread: &Mutex<Option<String>>,
    ) -> Result<String> {
        let job = &record.job;
        ensure_review_current(superseded)?;
        validate_repository(&job.repository)?;
        validate_sha(&job.base_ref)?;
        validate_sha(&job.head_sha)?;
        let token = self.installation_token(job.installation_id).await?;
        let repository_path = self
            .executor
            .sync_review_repository(&ReviewRepositorySync {
                root: self.data_dir.join("review-repositories"),
                repository: job.repository.clone(),
                pull_number: job.pull_number,
                base_sha: job.base_ref.clone(),
                head_sha: job.head_sha.clone(),
                token,
            })
            .await
            .map_err(|error| anyhow!(error))?;
        ensure_review_current(superseded)?;
        let workspace = self.register_workspace(
            &repository_path.to_string_lossy(),
            Some(job.repository.clone()),
        )?;
        let session = self
            .create_session(CreateSessionRequest {
                workspace_id: workspace.id,
                title: Some(format!("Review {} #{}", job.repository, job.pull_number)),
                base_ref: Some(job.base_ref.clone()),
                checkout_ref: Some(job.head_sha.clone()),
                fetch_latest: false,
            })
            .await?;
        let coordinator = self.create_thread(CreateThreadRequest {
            session_id: session.id.clone(),
            mode: Some("review".into()),
            model: job.model.clone(),
            model_options: serde_json::Map::new(),
            permission_mode: Some(PermissionMode::Yolo),
        })?;
        if !self
            .store
            .set_code_review_job_session(&job.id, &session.id, &coordinator.id)?
        {
            if let Err(error) = self.delete_session(&session.id).await {
                self.record_review_error(format!(
                    "cleaning up superseded review job {} before dispatch: {error}",
                    job.id
                ));
            }
            bail!("stale: review was superseded before model dispatch");
        }
        self.emit_code_review_updated(Some(job.id.clone()))?;
        ensure_review_current(superseded)?;
        let diff_files = self
            .executor
            .review_repository_diff(&ReviewRepositoryDiff {
                worktree: session.worktree_path.clone().into(),
                base_sha: job.base_ref.clone(),
            })
            .await
            .map_err(|error| anyhow!(error))?;
        let batches = build_review_batches(&diff_files);
        let reviewers = if record.reviewers.is_empty() {
            self.resolve_code_review_reviewers(&crate::reviewers::default_reviewer_ids())?
        } else {
            record.reviewers.clone()
        };
        let mut candidates = Vec::new();
        for reviewer in &reviewers {
            for (batch_index, batch) in batches.iter().enumerate() {
                ensure_review_current(superseded)?;
                let thread = self.create_thread(CreateThreadRequest {
                    session_id: session.id.clone(),
                    mode: Some("review".into()),
                    model: reviewer.model.clone().or_else(|| job.model.clone()),
                    model_options: reviewer_model_options(reviewer),
                    permission_mode: Some(PermissionMode::Yolo),
                })?;
                let output = self
                    .run_tracked_code_review_turn(
                        job,
                        &thread.id,
                        reviewer_prompt(record, reviewer, batch, batch_index, batches.len()),
                        superseded,
                        active_thread,
                    )
                    .await?;
                let parsed = parse_review_output(&output)?;
                candidates.extend(parsed.findings.into_iter().map(|finding| CandidateFinding {
                    reviewer_id: reviewer.id.clone(),
                    reviewer_name: reviewer.name.clone(),
                    finding,
                }));
                if candidates.len() > MAX_CANDIDATE_FINDINGS {
                    bail!(
                        "reviewers returned more than {MAX_CANDIDATE_FINDINGS} candidate findings"
                    );
                }
            }
        }
        let candidates = structurally_valid_candidates(candidates, &diff_files);
        let parsed = if candidates.is_empty() {
            ReviewOutput {
                summary: format!(
                    "{} reviewer(s) examined {} changed file(s); no actionable issues were confirmed.",
                    reviewers.len(),
                    diff_files.len()
                ),
                findings: Vec::new(),
            }
        } else {
            let output = self
                .run_tracked_code_review_turn(
                    job,
                    &coordinator.id,
                    validation_prompt(record, &candidates, &diff_files)?,
                    superseded,
                    active_thread,
                )
                .await?;
            let validated = parse_review_output(&output)?;
            ReviewOutput {
                summary: validated.summary,
                findings: structurally_valid_findings(validated.findings, &diff_files),
            }
        };

        let api = self.installation_api(job.installation_id).await?;
        let (current, rate): (GithubPullRequest, _) = api
            .get(&format!(
                "/repos/{}/pulls/{}",
                job.repository, job.pull_number
            ))
            .await?;
        self.record_review_rate(rate);
        if current.state != "open"
            || current.base.sha != job.base_ref
            || current.head.sha != job.head_sha
        {
            bail!("stale: pull request revision changed before the review was published");
        }
        let review_url = self.publish_review(&api, job, parsed).await?;
        Ok(review_url)
    }

    async fn run_tracked_code_review_turn(
        self: &Arc<Self>,
        job: &trouve_protocol::CodeReviewJob,
        thread_id: &str,
        prompt: String,
        superseded: &CancellationToken,
        active_thread: &Mutex<Option<String>>,
    ) -> Result<String> {
        *active_thread.lock().unwrap() = Some(thread_id.to_owned());
        let result = self
            .run_code_review_turn(job, thread_id, prompt, superseded)
            .await;
        let mut active_thread = active_thread.lock().unwrap();
        if active_thread.as_deref() == Some(thread_id) {
            *active_thread = None;
        }
        result
    }

    async fn run_code_review_turn(
        self: &Arc<Self>,
        job: &trouve_protocol::CodeReviewJob,
        thread_id: &str,
        prompt: String,
        superseded: &CancellationToken,
    ) -> Result<String> {
        ensure_review_current(superseded)?;
        let scope = Scope::Thread(thread_id.to_string());
        let mut events = self.store.subscribe();
        let mut after = self
            .store
            .events_after(&scope, 0)?
            .last()
            .map(|event| event.cursor)
            .unwrap_or(0);
        let mut replay = VecDeque::new();
        let accepted = self.send_message(thread_id, prompt, Vec::new())?;
        let turn = accepted.turn;
        let mut output = String::new();
        let mut cancellation_requested = false;
        loop {
            if superseded.is_cancelled() && !cancellation_requested {
                let _ = self.cancel_turn(thread_id);
                cancellation_requested = true;
            }
            let envelope = match replay.pop_front() {
                Some(envelope) => envelope,
                None => match if cancellation_requested {
                    events.recv().await
                } else {
                    tokio::select! {
                        received = events.recv() => received,
                        _ = superseded.cancelled() => {
                            let _ = self.cancel_turn(thread_id);
                            cancellation_requested = true;
                            continue;
                        }
                    }
                } {
                    Ok(envelope) => envelope,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            job_id = %job.id,
                            skipped,
                            "review event receiver lagged; replaying persisted events"
                        );
                        replay = VecDeque::from(
                            self.store
                                .events_after(&scope, after)
                                .context("replaying review events after receiver lag")?,
                        );
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        bail!("review event stream closed");
                    }
                },
            };
            if envelope.scope != scope || envelope.cursor <= after {
                continue;
            }
            after = envelope.cursor;
            match envelope.event {
                Event::AssistantMessage {
                    turn: event_turn,
                    content,
                } if event_turn == turn => output = content,
                Event::QuestionRequested { request_id, .. } => {
                    let _ = self.resolve_question(&request_id, None);
                }
                Event::TurnCompleted {
                    turn: event_turn, ..
                } if event_turn == turn => break,
                Event::TurnFailed {
                    turn: event_turn,
                    error,
                } if event_turn == turn => bail!("model review failed: {error}"),
                Event::TurnCancelled { turn: event_turn } if event_turn == turn => {
                    if superseded.is_cancelled() {
                        bail!("stale: review was superseded while the model was running");
                    }
                    bail!("model review was cancelled");
                }
                _ => {}
            }
        }
        ensure_review_current(superseded)?;
        if output.trim().is_empty() {
            bail!("model returned an empty review");
        }
        Ok(output)
    }

    async fn publish_review(
        &self,
        api: &GithubApi,
        job: &trouve_protocol::CodeReviewJob,
        review: ReviewOutput,
    ) -> Result<String> {
        let summary = if review.summary.trim().is_empty() {
            if review.findings.is_empty() {
                "No actionable issues found.".to_string()
            } else {
                format!("Found {} actionable issue(s).", review.findings.len())
            }
        } else {
            review.summary
        };
        let comments: Vec<_> = review
            .findings
            .iter()
            .filter(|finding| finding.line > 0 && !finding.path.trim().is_empty())
            .map(|finding| {
                let severity = finding.severity.trim();
                let body = if severity.is_empty() {
                    finding.body.clone()
                } else {
                    format!("**{}** — {}", severity.to_ascii_uppercase(), finding.body)
                };
                serde_json::json!({
                    "path": finding.path,
                    "line": finding.line,
                    "side": if finding.side.eq_ignore_ascii_case("LEFT") { "LEFT" } else { "RIGHT" },
                    "body": body,
                })
            })
            .collect();
        let path = format!(
            "/repos/{}/pulls/{}/reviews",
            job.repository, job.pull_number
        );
        let request = serde_json::json!({
            "commit_id": job.head_sha,
            "body": format!("{}\n\n_Reviewed by trouve._", summary),
            "event": "COMMENT",
            "comments": comments,
        });
        let response = api
            .request(reqwest::Method::POST, &path)
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        let rate = rate_info(response.headers());
        let body = response.text().await?;
        self.record_review_rate(rate);
        if status.is_success() {
            return Ok(serde_json::from_str::<PublishedReview>(&body)?.html_url);
        }
        if status.as_u16() != 422 || comments.is_empty() {
            bail!("GitHub API {status}: {}", compact_api_error(&body));
        }

        // A model can name a line that is not commentable in GitHub's diff.
        // Preserve the review instead of failing it: fold findings into the
        // summary and retry without inline comments.
        let mut fallback = summary;
        fallback.push_str("\n\n");
        for finding in review.findings {
            fallback.push_str(&format!(
                "- `{}` line {} [{}]: {}\n",
                finding.path,
                finding.line,
                finding.severity.to_ascii_uppercase(),
                finding.body
            ));
        }
        let (published, rate): (PublishedReview, _) = api
            .post(
                &path,
                &serde_json::json!({
                    "commit_id": job.head_sha,
                    "body": format!("{}\n\n_Reviewed by trouve._", fallback),
                    "event": "COMMENT",
                }),
            )
            .await?;
        self.record_review_rate(rate);
        Ok(published.html_url)
    }
}

fn apply_reviewer_overrides(
    reviewers: Vec<ReviewerProfile>,
    overrides: &[ReviewerOverride],
) -> Vec<ReviewerProfile> {
    let overrides: HashMap<_, _> = overrides
        .iter()
        .map(|reviewer_override| (reviewer_override.reviewer_id.as_str(), reviewer_override))
        .collect();
    reviewers
        .into_iter()
        .map(|mut reviewer| {
            let Some(reviewer_override) = overrides.get(reviewer.id.as_str()) else {
                return reviewer;
            };
            if let Some(model) = &reviewer_override.model {
                reviewer.model = Some(model.clone());
            }
            match reviewer_override.prompt_mode {
                ReviewerPromptMode::Inherit => {}
                ReviewerPromptMode::Append => {
                    reviewer.prompt = format!(
                        "{}\n\nRepository-specific reviewer instructions:\n{}",
                        reviewer.prompt.trim(),
                        reviewer_override.prompt
                    );
                }
                ReviewerPromptMode::Replace => {
                    reviewer.prompt = reviewer_override.prompt.clone();
                }
            }
            reviewer
        })
        .collect()
}

fn reviewer_model_options(
    reviewer: &ReviewerProfile,
) -> serde_json::Map<String, serde_json::Value> {
    reviewer
        .default_thinking_level
        .as_ref()
        .map(|level| {
            serde_json::Map::from_iter([(
                "thinking_level".into(),
                serde_json::Value::String(level.clone()),
            )])
        })
        .unwrap_or_default()
}

fn ensure_review_current(superseded: &CancellationToken) -> Result<()> {
    if superseded.is_cancelled() {
        bail!("stale: review was superseded by a newer revision or review configuration");
    }
    Ok(())
}

fn should_replace_manual_review(
    mode: CodeReviewMode,
    review_superseded: bool,
    manual_requested: bool,
    generation: Option<u64>,
) -> bool {
    mode == CodeReviewMode::Manual && review_superseded && manual_requested && generation.is_none()
}

fn build_review_batches(files: &[ReviewDiffFile]) -> Vec<ReviewBatch> {
    if files.is_empty() {
        return vec![ReviewBatch {
            paths: Vec::new(),
            diff: "No textual file changes were reported by git.".into(),
        }];
    }
    let mut batches = Vec::new();
    let mut current = ReviewBatch {
        paths: Vec::new(),
        diff: String::new(),
    };
    for file in files {
        // Reserve enough room for the repeated path/fragment header so even
        // one very large file cannot produce an oversized model request.
        let largest_header = format!("\n=== {} (diff fragment {}) ===\n", file.path, usize::MAX);
        let chunk_limit = REVIEW_BATCH_MAX_BYTES
            .saturating_sub(largest_header.len() + 1)
            .max(1);
        let chunks = split_diff_chunks(&file.diff, chunk_limit);
        for (index, chunk) in chunks.into_iter().enumerate() {
            let section = format!(
                "\n=== {} (diff fragment {}) ===\n{}\n",
                file.path,
                index + 1,
                chunk
            );
            if !current.diff.is_empty()
                && (current.diff.len() + section.len() > REVIEW_BATCH_MAX_BYTES
                    || current.paths.len() >= REVIEW_BATCH_MAX_FILES)
            {
                batches.push(current);
                current = ReviewBatch {
                    paths: Vec::new(),
                    diff: String::new(),
                };
            }
            if !current.paths.contains(&file.path) {
                current.paths.push(file.path.clone());
            }
            current.diff.push_str(&section);
        }
    }
    if !current.diff.is_empty() {
        batches.push(current);
    }
    batches
}

fn split_diff_chunks(diff: &str, limit: usize) -> Vec<&str> {
    if diff.is_empty() {
        return vec![diff];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < diff.len() {
        let mut end = start.saturating_add(limit).min(diff.len());
        while end > start && !diff.is_char_boundary(end) {
            end -= 1;
        }
        if end < diff.len()
            && let Some(last_newline) = diff[start..end].rfind('\n')
            && last_newline >= limit / 2
        {
            end = start + last_newline + 1;
        }
        if end == start {
            end = diff[start..]
                .char_indices()
                .nth(1)
                .map(|(offset, _)| start + offset)
                .unwrap_or(diff.len());
        }
        chunks.push(&diff[start..end]);
        start = end;
    }
    chunks
}

fn reviewer_prompt(
    record: &CodeReviewJobRecord,
    reviewer: &ReviewerProfile,
    batch: &ReviewBatch,
    batch_index: usize,
    batch_count: usize,
) -> String {
    let job = &record.job;
    let extra = if record.prompt.trim().is_empty() {
        String::new()
    } else {
        format!("\nRepository-specific instructions:\n{}\n", record.prompt)
    };
    format!(
        "You are the `{reviewer_name}` reviewer. Your focused mandate is:\n\
         {reviewer_instructions}\n\nReview pull request #{number} ({title}) at immutable head {head}, \
         compared with base commit {base}. This is complete diff batch {batch_number} of \
         {batch_count}; review every supplied file or fragment. Inspect relevant unchanged \
         code with read/search tools when needed. Report only concrete, actionable problems \
         introduced by the change. Do not ask questions and do not modify files.\n\
         {extra}\nChanged paths in this batch: {paths}\n\nUnified diff:\n{diff}\n\n\
         Return JSON only, with no Markdown fence, using exactly this shape:\n\
         {{\"summary\":\"short overall assessment\",\"findings\":[{{\"path\":\"relative/file.rs\",\"line\":123,\"side\":\"RIGHT\",\"severity\":\"high|medium|low\",\"body\":\"specific problem and fix\"}}]}}\n\
         Use RIGHT for added/context lines in the new version and LEFT only \
         for removed lines. Return an empty findings array when there are no \
         actionable issues.",
        reviewer_name = reviewer.name,
        reviewer_instructions = reviewer.prompt,
        number = job.pull_number,
        title = job.pull_title,
        head = job.head_sha,
        base = job.base_ref,
        batch_number = batch_index + 1,
        batch_count = batch_count,
        paths = batch.paths.join(", "),
        diff = batch.diff,
    )
}

fn validation_prompt(
    record: &CodeReviewJobRecord,
    candidates: &[CandidateFinding],
    files: &[ReviewDiffFile],
) -> Result<String> {
    let job = &record.job;
    let candidates = serde_json::to_string_pretty(candidates)?;
    let paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let extra = if record.prompt.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Repository-specific review instructions:\n{}\n\n",
            record.prompt
        )
    };
    Ok(format!(
        "Act as the final code-review editor for pull request #{number} ({title}) at \
         immutable revision {base}..{head}. Independently verify every candidate against \
         the diff and repository. Remove false positives, issues not introduced by this \
         revision, non-actionable style preferences, and duplicates. Merge overlapping \
         findings, correct path/side/line metadata, normalize severity to high/medium/low, \
         and retain only findings a maintainer should act on. Use git_diff with base `{base}` \
         and its path/offset pagination when exact removed or context lines need verification; \
         use read/search tools for surrounding code. Do not add a finding merely because an \
         reviewer suggested it.\n\n{extra}Changed paths: {paths}\n\nCandidate findings:\n{candidates}\n\n\
         Return JSON only, with no Markdown fence, using exactly this shape:\n\
         {{\"summary\":\"concise final assessment that mentions validated coverage\",\"findings\":[{{\"path\":\"relative/file.rs\",\"line\":123,\"side\":\"RIGHT\",\"severity\":\"high|medium|low\",\"body\":\"specific verified problem and fix\"}}]}}",
        number = job.pull_number,
        title = job.pull_title,
        base = job.base_ref,
        head = job.head_sha,
    ))
}

fn structurally_valid_candidates(
    candidates: Vec<CandidateFinding>,
    files: &[ReviewDiffFile],
) -> Vec<CandidateFinding> {
    let valid = diff_comment_lines(files);
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|mut candidate| {
            normalize_finding(&mut candidate.finding, &valid)?;
            let key = finding_key(&candidate.finding);
            seen.insert(key).then_some(candidate)
        })
        .collect()
}

fn structurally_valid_findings(
    findings: Vec<ReviewFinding>,
    files: &[ReviewDiffFile],
) -> Vec<ReviewFinding> {
    let valid = diff_comment_lines(files);
    let mut seen = HashSet::new();
    findings
        .into_iter()
        .filter_map(|mut finding| {
            normalize_finding(&mut finding, &valid)?;
            let key = finding_key(&finding);
            seen.insert(key).then_some(finding)
        })
        .collect()
}

fn finding_key(finding: &ReviewFinding) -> (String, u64, String, String) {
    (
        finding.path.clone(),
        finding.line,
        finding.side.clone(),
        finding
            .body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase(),
    )
}

fn normalize_finding(
    finding: &mut ReviewFinding,
    valid: &HashSet<(String, u64, bool)>,
) -> Option<()> {
    finding.path = finding
        .path
        .trim()
        .strip_prefix("a/")
        .or_else(|| finding.path.trim().strip_prefix("b/"))
        .unwrap_or(finding.path.trim())
        .to_string();
    finding.body = finding.body.trim().chars().take(4_000).collect();
    if finding.path.is_empty() || finding.line == 0 || finding.body.is_empty() {
        return None;
    }
    let mut left = finding.side.eq_ignore_ascii_case("LEFT");
    if !valid.contains(&(finding.path.clone(), finding.line, left)) {
        if valid.contains(&(finding.path.clone(), finding.line, !left)) {
            left = !left;
        } else {
            return None;
        }
    }
    finding.side = if left { "LEFT" } else { "RIGHT" }.into();
    finding.severity = match finding.severity.trim().to_ascii_lowercase().as_str() {
        "high" => "high",
        "low" => "low",
        _ => "medium",
    }
    .into();
    Some(())
}

/// (path, line, left-side). Context lines are commentable on either side;
/// additions are RIGHT and removals are LEFT.
fn diff_comment_lines(files: &[ReviewDiffFile]) -> HashSet<(String, u64, bool)> {
    let mut valid = HashSet::new();
    for file in files {
        let mut old_line = 0;
        let mut new_line = 0;
        let mut in_hunk = false;
        for line in file.diff.lines() {
            if line.starts_with("@@ ") {
                let mut ranges = line.split_whitespace();
                let _marker = ranges.next();
                old_line = ranges
                    .next()
                    .and_then(|range| diff_range_start(range, '-'))
                    .unwrap_or(0);
                new_line = ranges
                    .next()
                    .and_then(|range| diff_range_start(range, '+'))
                    .unwrap_or(0);
                in_hunk = old_line > 0 || new_line > 0;
                continue;
            }
            if !in_hunk || line.starts_with("\\ No newline at end of file") {
                continue;
            }
            match line.as_bytes().first().copied() {
                Some(b'+') => {
                    valid.insert((file.path.clone(), new_line, false));
                    new_line += 1;
                }
                Some(b'-') => {
                    valid.insert((file.path.clone(), old_line, true));
                    old_line += 1;
                }
                Some(b' ') => {
                    valid.insert((file.path.clone(), old_line, true));
                    valid.insert((file.path.clone(), new_line, false));
                    old_line += 1;
                    new_line += 1;
                }
                _ => in_hunk = false,
            }
        }
    }
    valid
}

fn diff_range_start(range: &str, prefix: char) -> Option<u64> {
    range.strip_prefix(prefix)?.split(',').next()?.parse().ok()
}

fn parse_review_output(output: &str) -> Result<ReviewOutput> {
    let trimmed = output.trim();
    if let Ok(review) = serde_json::from_str(trimmed) {
        return Ok(review);
    }
    let start = trimmed
        .find('{')
        .ok_or_else(|| anyhow!("review did not contain JSON"))?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| anyhow!("review did not contain JSON"))?;
    serde_json::from_str(&trimmed[start..=end]).context("decoding model review JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_review_json() {
        let review =
            parse_review_output("```json\n{\"summary\":\"ok\",\"findings\":[]}\n```").unwrap();
        assert_eq!(review.summary, "ok");
        assert!(review.findings.is_empty());
    }

    #[test]
    fn reviewer_overrides_append_or_replace_prompts_and_models() {
        let reviewer = ReviewerProfile {
            id: "security".into(),
            name: "Security".into(),
            prompt: "Check trust boundaries.".into(),
            model: Some("openai/base".into()),
            default_thinking_level: Some("high".into()),
            built_in: true,
        };
        let appended = apply_reviewer_overrides(
            vec![reviewer.clone()],
            &[ReviewerOverride {
                reviewer_id: "security".into(),
                model: Some("anthropic/reviewer".into()),
                prompt_mode: ReviewerPromptMode::Append,
                prompt: "Focus on tenant isolation.".into(),
            }],
        );
        assert_eq!(appended[0].model.as_deref(), Some("anthropic/reviewer"));
        assert_eq!(appended[0].default_thinking_level.as_deref(), Some("high"));
        assert!(appended[0].prompt.starts_with(&reviewer.prompt));
        assert!(appended[0].prompt.ends_with("Focus on tenant isolation."));

        let replaced = apply_reviewer_overrides(
            vec![reviewer],
            &[ReviewerOverride {
                reviewer_id: "security".into(),
                model: None,
                prompt_mode: ReviewerPromptMode::Replace,
                prompt: "Review only authorization changes.".into(),
            }],
        );
        assert_eq!(replaced[0].model.as_deref(), Some("openai/base"));
        assert_eq!(replaced[0].prompt, "Review only authorization changes.");
    }

    #[test]
    fn reviewer_thinking_default_becomes_a_canonical_thread_option() {
        let mut reviewer = crate::reviewers::built_in_reviewers().remove(0);
        assert!(reviewer_model_options(&reviewer).is_empty());

        reviewer.default_thinking_level = Some("high".into());
        assert_eq!(
            reviewer_model_options(&reviewer).get("thinking_level"),
            Some(&serde_json::json!("high"))
        );
    }

    #[test]
    fn built_in_reviewer_model_and_thinking_defaults_can_be_customized() {
        let data = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open_in_memory().unwrap();
        let engine = Engine::new(
            store,
            data.path().to_path_buf(),
            &crate::config::Config::default(),
        );
        let saved = engine
            .upsert_reviewer_profile(UpsertReviewerProfileRequest {
                id: Some("security".into()),
                // Built-in content remains canonical even if a client sends
                // stale display data while changing its defaults.
                name: "stale".into(),
                prompt: "stale".into(),
                model: Some("anthropic/claude-sonnet".into()),
                default_thinking_level: Some("high".into()),
            })
            .unwrap();

        assert!(saved.built_in);
        assert_eq!(saved.name, "Security & Privacy");
        assert_ne!(saved.prompt, "stale");
        assert_eq!(saved.model.as_deref(), Some("anthropic/claude-sonnet"));
        assert_eq!(saved.default_thinking_level.as_deref(), Some("high"));

        let catalog = engine.code_review_reviewer_catalog().unwrap();
        let security = catalog
            .iter()
            .find(|reviewer| reviewer.id == "security")
            .unwrap();
        assert_eq!(security, &saved);
    }

    #[test]
    fn review_batches_cover_every_file_and_bound_large_diffs() {
        let large = format!("{}{}", "a".repeat(REVIEW_BATCH_MAX_BYTES), "β".repeat(20));
        let chunks = split_diff_chunks(&large, REVIEW_BATCH_MAX_BYTES);
        assert_eq!(chunks.concat(), large);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.len() <= REVIEW_BATCH_MAX_BYTES)
        );

        let files = vec![
            ReviewDiffFile {
                path: "src/large.rs".into(),
                diff: large,
            },
            ReviewDiffFile {
                path: "src/small.rs".into(),
                diff: "+small\n".into(),
            },
        ];
        let batches = build_review_batches(&files);
        let covered: HashSet<_> = batches
            .iter()
            .flat_map(|batch| batch.paths.iter().map(String::as_str))
            .collect();
        assert_eq!(covered, HashSet::from(["src/large.rs", "src/small.rs"]));
        assert!(batches.len() >= 2);
        assert!(
            batches
                .iter()
                .all(|batch| batch.diff.len() <= REVIEW_BATCH_MAX_BYTES)
        );
    }

    #[test]
    fn structural_validation_fixes_sides_and_deduplicates_candidates() {
        let files = vec![ReviewDiffFile {
            path: "src/lib.rs".into(),
            diff: "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -20,2 +2,3 @@\n context\n+added\n tail\n".into(),
        }];
        let candidate = |path: &str, side: &str, body: &str| CandidateFinding {
            reviewer_id: "correctness".into(),
            reviewer_name: "Correctness".into(),
            finding: ReviewFinding {
                path: path.into(),
                line: 3,
                side: side.into(),
                severity: "critical".into(),
                body: body.into(),
            },
        };
        let valid = structurally_valid_candidates(
            vec![
                candidate("b/src/lib.rs", "LEFT", "real issue"),
                candidate("src/lib.rs", "RIGHT", "real issue"),
                candidate("src/other.rs", "RIGHT", "not in diff"),
            ],
            &files,
        );
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0].finding.path, "src/lib.rs");
        assert_eq!(valid[0].finding.side, "RIGHT");
        assert_eq!(valid[0].finding.severity, "medium");
    }

    #[test]
    fn validates_repository_names_and_shas() {
        assert!(validate_repository("owner/repo-name").is_ok());
        assert!(validate_repository("../repo").is_err());
        assert!(validate_repository("owner/repo/extra").is_err());
        assert!(validate_sha("0123456789012345678901234567890123456789").is_ok());
        assert!(validate_sha("main").is_err());
    }

    #[test]
    fn poll_interval_values_must_be_positive_seconds() {
        assert_eq!(DEFAULT_RECONCILE_INTERVAL, Duration::from_secs(60));
        assert_eq!(
            parse_code_review_poll_interval(" 15 "),
            Some(Duration::from_secs(15))
        );
        for value in ["", "0", "nope"] {
            assert_eq!(parse_code_review_poll_interval(value), None);
        }
    }

    #[test]
    fn manual_review_command_must_be_on_its_own_line() {
        for body in [
            "@trouve-ai review",
            "  @TROUVE-AI   REVIEW  ",
            "Context before\n@trouve-ai review\nContext after",
        ] {
            assert!(contains_manual_review_command(body), "{body:?}");
        }
        for body in [
            "@trouve-ai reviews",
            "please @trouve-ai review",
            "@trouve-ai review this",
            "`@trouve-ai review`",
        ] {
            assert!(!contains_manual_review_command(body), "{body:?}");
        }
    }

    #[test]
    fn trusted_pr_comments_create_stable_manual_review_requests() {
        let mut payload = serde_json::json!({
            "action": "created",
            "installation": {"id": 7},
            "repository": {"full_name": "acme/widgets"},
            "issue": {
                "number": 42,
                "pull_request": {"url": "https://api.github.com/repos/acme/widgets/pulls/42"}
            },
            "comment": {
                "id": 100,
                "body": "@trouve-ai review",
                "author_association": "MEMBER",
                "user": {"type": "User"}
            }
        });
        assert_eq!(
            manual_review_comment(&payload),
            Some(ManualReviewComment {
                repository: "acme/widgets".into(),
                installation_id: 7,
                pull_number: 42,
                trigger_key: "manual:comment:100".into(),
            })
        );

        payload["comment"]["author_association"] = serde_json::json!("CONTRIBUTOR");
        assert_eq!(manual_review_comment(&payload), None);
        payload["comment"]["author_association"] = serde_json::json!("OWNER");
        payload["comment"]["user"]["type"] = serde_json::json!("Bot");
        assert_eq!(manual_review_comment(&payload), None);
        payload["comment"]["user"]["type"] = serde_json::json!("User");
        payload["issue"]["pull_request"] = serde_json::Value::Null;
        assert_eq!(manual_review_comment(&payload), None);
    }

    #[test]
    fn comment_requests_trigger_manual_reviews_even_for_drafts() {
        let comments = vec![CodeReviewManualRequest {
            pull_number: 42,
            trigger_key: "manual:comment:100".into(),
        }];
        assert_eq!(
            requested_review_triggers(CodeReviewMode::Manual, true, None, false, &comments),
            vec![RequestedReviewTrigger {
                requested_key: "manual:comment:100".into(),
                trigger: "manual",
                comment_key: Some("manual:comment:100".into()),
            }]
        );
        assert!(
            requested_review_triggers(CodeReviewMode::Manual, false, None, false, &[]).is_empty()
        );
        assert_eq!(
            requested_review_triggers(CodeReviewMode::Automatic, false, None, false, &[]),
            vec![RequestedReviewTrigger {
                requested_key: "automatic".into(),
                trigger: "automatic",
                comment_key: None,
            }]
        );
    }

    #[test]
    fn draft_manual_requests_keep_their_stable_dedupe_key() {
        assert!(!manual_request_can_satisfy_automatic_review(
            CodeReviewMode::Automatic,
            true,
            "manual"
        ));
        assert!(manual_request_can_satisfy_automatic_review(
            CodeReviewMode::Automatic,
            false,
            "manual"
        ));
        assert!(!manual_request_can_satisfy_automatic_review(
            CodeReviewMode::Manual,
            false,
            "manual"
        ));
    }

    #[test]
    fn outstanding_manual_request_replaces_a_superseded_review() {
        assert!(should_replace_manual_review(
            CodeReviewMode::Manual,
            true,
            true,
            None
        ));
        assert!(!should_replace_manual_review(
            CodeReviewMode::Automatic,
            true,
            true,
            None
        ));
        assert!(!should_replace_manual_review(
            CodeReviewMode::Manual,
            false,
            true,
            None
        ));
        assert!(!should_replace_manual_review(
            CodeReviewMode::Manual,
            true,
            true,
            Some(2)
        ));
    }

    #[test]
    fn superseded_job_cancels_the_running_review() {
        let runtime = CodeReviewRuntime::default();
        let cancel = CancellationToken::new();
        *runtime.running.lock().unwrap() = Some(RunningReview {
            job_id: "rv_old".into(),
            cancel: cancel.clone(),
        });

        runtime.cancel_superseded(&["rv_other".into()]);
        assert!(!cancel.is_cancelled());
        runtime.cancel_superseded(&["rv_old".into()]);
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn webhook_signatures_are_verified_and_deliveries_are_idempotent() {
        let data = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open_in_memory().unwrap();
        let config = crate::config::Config {
            github_review_app: Some(GithubReviewAppConfig {
                app_id: 7,
                slug: "trouve-review".into(),
            }),
            ..Default::default()
        };
        let mut engine = Engine::new(store, data.path().to_path_buf(), &config);
        engine.secrets = Arc::new(trouve_providers::secrets::FileStore::new(
            data.path().join("secrets.json"),
        ));
        let engine = Arc::new(engine);
        engine.secrets.set(WEBHOOK_SECRET, "shared-secret").unwrap();
        let body = br#"{"zen":"keep it logically awesome"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"shared-secret").unwrap();
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(
            engine
                .accept_github_review_webhook("ping", "delivery-1", "sha256=00", body)
                .is_err()
        );
        engine
            .accept_github_review_webhook("ping", "delivery-1", &signature, body)
            .unwrap();
        engine
            .accept_github_review_webhook("ping", "delivery-1", &signature, body)
            .unwrap();
    }

    #[tokio::test]
    async fn trusted_comment_webhook_durably_records_manual_request() {
        let data = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open_in_memory().unwrap();
        store
            .upsert_discovered_code_review_repository(7, "acme/widgets", false)
            .unwrap();
        store
            .update_code_review_repository(&UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/widgets".into(),
                mode: CodeReviewMode::Manual,
                model: None,
                prompt: String::new(),
                reviewer_ids: None,
                reviewer_overrides: None,
            })
            .unwrap();
        let config = crate::config::Config {
            github_review_app: Some(GithubReviewAppConfig {
                app_id: 7,
                slug: "trouve-review".into(),
            }),
            ..Default::default()
        };
        let mut engine = Engine::new(store, data.path().to_path_buf(), &config);
        engine.secrets = Arc::new(trouve_providers::secrets::FileStore::new(
            data.path().join("secrets.json"),
        ));
        let engine = Arc::new(engine);
        engine.secrets.set(WEBHOOK_SECRET, "shared-secret").unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "action": "created",
            "installation": {"id": 7},
            "repository": {"full_name": "acme/widgets"},
            "issue": {
                "number": 42,
                "pull_request": {"url": "https://api.github.com/repos/acme/widgets/pulls/42"}
            },
            "comment": {
                "id": 100,
                "body": "@trouve-ai review",
                "author_association": "OWNER",
                "user": {"type": "User"}
            }
        }))
        .unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"shared-secret").unwrap();
        mac.update(&body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        // Keep the spawned reconciliation behind the lock so this assertion
        // specifically verifies the synchronous durable webhook handoff.
        let _reconcile_guard = engine.code_review.reconcile_lock.lock().await;
        engine
            .accept_github_review_webhook("issue_comment", "delivery-comment-1", &signature, &body)
            .unwrap();
        assert_eq!(
            engine
                .store
                .pending_code_review_manual_requests("acme/widgets")
                .unwrap(),
            vec![CodeReviewManualRequest {
                pull_number: 42,
                trigger_key: "manual:comment:100".into(),
            }]
        );
    }
}
