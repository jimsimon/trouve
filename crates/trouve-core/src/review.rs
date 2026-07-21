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
    CreateSessionRequest, CreateThreadRequest, Event, GithubAppStatus, PermissionMode, Scope,
    UpdateCodeReviewRepositoryRequest,
};

use crate::config::GithubReviewAppConfig;
use crate::engine::{Engine, EngineError};
use crate::store::{CodeReviewJobRecord, NewCodeReviewJob};
use crate::tools::ReviewRepositorySync;

const PRIVATE_KEY_SECRET: &str = "github:review-app:private-key";
const WEBHOOK_SECRET: &str = "github:review-app:webhook-secret";
const RECONCILE_INTERVAL_ENV: &str = "TROUVE_CODE_REVIEW_POLL_INTERVAL_SECONDS";
const DEFAULT_RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
const JOB_IDLE_INTERVAL: Duration = Duration::from_secs(5);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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

#[derive(Deserialize)]
struct PublishedReview {
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct ReviewOutput {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    findings: Vec<ReviewFinding>,
}

#[derive(Debug, Deserialize)]
struct ReviewFinding {
    path: String,
    line: u64,
    #[serde(default = "default_review_side")]
    side: String,
    #[serde(default)]
    severity: String,
    body: String,
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
            repositories: self.store.list_code_review_repositories()?,
            jobs: self.store.list_code_review_jobs(100)?,
        })
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
        self.store.update_code_review_repository(request)?;
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
        let mut installations = Vec::new();
        let mut installation_page = 1;
        loop {
            let (page, rate): (Vec<Installation>, _) = api
                .get(&format!(
                    "/app/installations?per_page=100&page={installation_page}"
                ))
                .await
                .context("listing GitHub App installations")?;
            self.record_review_rate(rate);
            let count = page.len();
            installations.extend(page);
            if count < 100 {
                break;
            }
            installation_page += 1;
        }
        {
            let mut state = self.code_review.state.lock().unwrap();
            state.installation_count = installations.len() as u64;
        }

        let mut active_repositories = HashSet::new();
        for installation in installations {
            let installation_api = self.installation_api(installation.id).await?;
            let mut page = 1;
            loop {
                let (repositories, rate): (InstallationRepositories, _) = installation_api
                    .get(&format!(
                        "/installation/repositories?per_page=100&page={page}"
                    ))
                    .await
                    .context("listing installation repositories")?;
                self.record_review_rate(rate);
                let count = repositories.repositories.len();
                for repository in repositories.repositories {
                    active_repositories.insert((installation.id, repository.full_name.clone()));
                    self.store.upsert_discovered_code_review_repository(
                        installation.id,
                        &repository.full_name,
                        repository.private,
                    )?;
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
            self.poll_code_review_repository(repository).await?;
        }
        {
            let mut state = self.code_review.state.lock().unwrap();
            state.last_poll_at = Some(Utc::now());
            state.last_error.clear();
        }
        self.emit_code_review_updated(None)?;
        Ok(())
    }

    async fn poll_code_review_repository(&self, repository: &CodeReviewRepository) -> Result<()> {
        validate_repository(&repository.repository)?;
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
        for pull in pulls {
            validate_sha(&pull.base.sha)?;
            validate_sha(&pull.head.sha)?;
            let superseded = self.store.supersede_code_review_jobs(
                &repository.repository,
                pull.number,
                &pull.base.sha,
                &pull.head.sha,
            )?;
            let revision_changed = !superseded.is_empty();
            if revision_changed {
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
            // still selected, replace it for the new revision without
            // requiring the user to toggle the review request off and on.
            let replace_manual_review = should_replace_manual_review(
                repository.mode,
                revision_changed,
                manual_requested,
                generation,
            );
            if pull.draft && generation.is_none() && !replace_manual_review {
                continue;
            }
            let trigger = if generation.is_some() || replace_manual_review {
                "manual"
            } else if repository.mode == CodeReviewMode::Automatic {
                "automatic"
            } else {
                continue;
            };
            let config_hash = hex::encode(Sha256::digest(
                format!("{:?}\0{}", repository.model, repository.prompt).as_bytes(),
            ));
            let automatic_key = format!(
                "{}#{}:{}:{}:automatic:{config_hash}",
                repository.repository, pull.number, pull.base.sha, pull.head.sha
            );
            // A manual request that arrives before this automatic head was
            // seen satisfies the automatic review too. Later re-requests get
            // their own generation and intentionally run again.
            let identity = if let Some(generation) = generation {
                if repository.mode == CodeReviewMode::Automatic
                    && !self.store.code_review_job_exists(&automatic_key)?
                {
                    "automatic".into()
                } else {
                    format!("manual:{generation}")
                }
            } else if replace_manual_review {
                "manual:revision".into()
            } else {
                "automatic".into()
            };
            let dedupe_key = format!(
                "{}#{}:{}:{}:{identity}:{config_hash}",
                repository.repository, pull.number, pull.base.sha, pull.head.sha
            );
            let job = self.store.enqueue_code_review_job(&NewCodeReviewJob {
                dedupe_key,
                installation_id: repository.installation_id,
                repository: repository.repository.clone(),
                pull_number: pull.number,
                pull_title: pull.title,
                pull_url: pull.html_url,
                head_sha: pull.head.sha,
                base_ref: pull.base.sha,
                head_ref: pull.head.name,
                trigger: trigger.into(),
                model: repository.model.clone(),
                prompt: repository.prompt.clone(),
            })?;
            if let Some(job) = job {
                self.emit_code_review_updated(Some(job.id))?;
                self.code_review.job_wake.notify_one();
            }
        }
        Ok(())
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
        if !self.store.claim_github_webhook_delivery(delivery_id)? {
            return Ok(());
        }
        if event != "pull_request" {
            return Ok(());
        }
        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|error| EngineError::BadRequest(format!("invalid webhook JSON: {error}")))?;
        let action = payload["action"].as_str().unwrap_or_default();
        if !matches!(
            action,
            "opened"
                | "reopened"
                | "synchronize"
                | "ready_for_review"
                | "review_requested"
                | "review_request_removed"
        ) {
            return Ok(());
        }
        let repository_name = payload["repository"]["full_name"]
            .as_str()
            .unwrap_or_default();
        let installation_id = payload["installation"]["id"].as_u64().unwrap_or_default();
        let repository = self
            .store
            .list_code_review_repositories()?
            .into_iter()
            .find(|repository| {
                repository.repository == repository_name
                    && repository.installation_id == installation_id
                    && repository.mode != CodeReviewMode::Off
            });
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
        let result =
            tokio::time::timeout(REVIEW_TIMEOUT, self.execute_code_review(&record, &cancel)).await;
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
        let thread = self.create_thread(CreateThreadRequest {
            session_id: session.id.clone(),
            mode: Some("review".into()),
            model: job.model.clone(),
            model_options: serde_json::Map::new(),
            permission_mode: Some(PermissionMode::Yolo),
        })?;
        if !self
            .store
            .set_code_review_job_session(&job.id, &session.id, &thread.id)?
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

        let scope = Scope::Thread(thread.id.clone());
        let mut events = self.store.subscribe();
        let mut after = self
            .store
            .events_after(&scope, 0)?
            .last()
            .map(|event| event.cursor)
            .unwrap_or(0);
        let mut replay = VecDeque::new();
        let accepted = self.send_message(&thread.id, review_prompt(record), Vec::new())?;
        let turn = accepted.turn;
        let mut output = String::new();
        let mut cancellation_requested = false;
        loop {
            if superseded.is_cancelled() && !cancellation_requested {
                let _ = self.cancel_turn(&thread.id);
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
                            let _ = self.cancel_turn(&thread.id);
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
                } if event_turn == turn => {
                    output = content;
                }
                Event::QuestionRequested { request_id, .. } => {
                    let _ = self.resolve_question(&request_id, None);
                }
                Event::TurnCompleted {
                    turn: event_turn, ..
                } if event_turn == turn => break,
                Event::TurnFailed {
                    turn: event_turn,
                    error,
                } if event_turn == turn => {
                    bail!("model review failed: {error}");
                }
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
        let parsed = parse_review_output(&output)?;
        let review_url = self.publish_review(&api, job, parsed).await?;
        Ok(review_url)
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

fn ensure_review_current(superseded: &CancellationToken) -> Result<()> {
    if superseded.is_cancelled() {
        bail!("stale: review was superseded by a newer pull request revision");
    }
    Ok(())
}

fn should_replace_manual_review(
    mode: CodeReviewMode,
    revision_changed: bool,
    manual_requested: bool,
    generation: Option<u64>,
) -> bool {
    mode == CodeReviewMode::Manual && revision_changed && manual_requested && generation.is_none()
}

fn review_prompt(record: &CodeReviewJobRecord) -> String {
    let job = &record.job;
    let extra = if record.prompt.trim().is_empty() {
        String::new()
    } else {
        format!("\nRepository-specific instructions:\n{}\n", record.prompt)
    };
    format!(
        "Review pull request #{number} ({title}) at immutable head {head}. \
         Compare it with base commit {base}. Start by calling git_diff with \
         base `{base}`, then inspect relevant files. Report only actionable \
         correctness, security, performance, or maintainability problems \
         introduced by this diff. Do not ask questions and do not modify files.\n\
         {extra}\nReturn JSON only, with no Markdown fence, using exactly this shape:\n\
         {{\"summary\":\"short overall assessment\",\"findings\":[{{\"path\":\"relative/file.rs\",\"line\":123,\"side\":\"RIGHT\",\"severity\":\"high|medium|low\",\"body\":\"specific problem and fix\"}}]}}\n\
         Use RIGHT for added/context lines in the new version and LEFT only \
         for removed lines. Return an empty findings array when there are no \
         actionable issues.",
        number = job.pull_number,
        title = job.pull_title,
        head = job.head_sha,
        base = job.base_ref,
    )
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
}
