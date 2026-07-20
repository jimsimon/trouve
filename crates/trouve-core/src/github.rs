//! GitHub PR integration: account and repository reads use batched GraphQL;
//! create and merge mutations use octocrab's REST handlers.
//!
//! Covered today: account-wide dashboard discovery, PR lookup by branch and
//! session activity, create (incl. draft), combined status (checks + reviews),
//! and merge with method selection. Merge queues and GitLab are tracked as
//! follow-ups.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use octocrab::Octocrab;
use serde::Deserialize;
use trouve_protocol::{CheckRun, PrInfo, PrReview};

/// A dashboard refresh should stay bounded even for repositories with an
/// unusually deep PR history. Each page contains up to 100 PRs.
const DASHBOARD_MAX_PR_PAGES: usize = 3;
const DASHBOARD_GRAPHQL_BATCH: usize = 50;

const VIEWER_QUERY: &str = r#"
query TrouveViewer {
  viewer { login }
  rateLimit { cost remaining resetAt }
}
"#;

const DASHBOARD_SEARCH_QUERY: &str = r#"
query TrouvePullRequestSearch(
  $openQuery: String!, $reviewQuery: String!, $mergedQuery: String!,
  $openAfter: String, $reviewAfter: String, $mergedAfter: String,
  $includeOpen: Boolean!, $includeReview: Boolean!, $includeMerged: Boolean!
) {
  open: search(query: $openQuery, type: ISSUE, first: 100, after: $openAfter)
    @include(if: $includeOpen) {
    nodes { ... on PullRequest { id } }
    pageInfo { hasNextPage endCursor }
  }
  review: search(query: $reviewQuery, type: ISSUE, first: 100, after: $reviewAfter)
    @include(if: $includeReview) {
    nodes { ... on PullRequest { id } }
    pageInfo { hasNextPage endCursor }
  }
  merged: search(query: $mergedQuery, type: ISSUE, first: 100, after: $mergedAfter)
    @include(if: $includeMerged) {
    nodes { ... on PullRequest { id } }
    pageInfo { hasNextPage endCursor }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const DASHBOARD_DETAILS_QUERY: &str = r#"
query TrouvePullRequestDetails($ids: [ID!]!) {
  nodes(ids: $ids) { ...TrouvePullRequestFields }
  rateLimit { cost remaining resetAt }
}
"#;

const BRANCH_PULL_REQUESTS_QUERY: &str = r#"
query TrouveBranchPullRequests(
  $owner: String!, $repository: String!, $branch: String!, $states: [PullRequestState!]
) {
  repository(owner: $owner, name: $repository) {
    pullRequests(
      first: 20,
      headRefName: $branch,
      states: $states,
      orderBy: { field: CREATED_AT, direction: DESC }
    ) {
      nodes { ...TrouvePullRequestFields }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const PULL_REQUEST_QUERY: &str = r#"
query TrouvePullRequest($owner: String!, $repository: String!, $number: Int!) {
  repository(owner: $owner, name: $repository) {
    pullRequest(number: $number) { ...TrouvePullRequestFields }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const OPEN_PULL_REQUESTS_QUERY: &str = r#"
query TrouveOpenPullRequests($owner: String!, $repository: String!, $after: String) {
  repository(owner: $owner, name: $repository) {
    pullRequests(
      first: 100,
      after: $after,
      states: [OPEN],
      orderBy: { field: CREATED_AT, direction: DESC }
    ) {
      nodes { ...TrouvePullRequestFields }
      pageInfo { hasNextPage endCursor }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const PULL_REQUEST_FIELDS: &str = r#"
fragment TrouvePullRequestFields on PullRequest {
  repository { nameWithOwner }
  headRepository { nameWithOwner }
  number
  url
  title
  state
  isDraft
  baseRefName
  headRefName
  headRefOid
  author { login }
  mergeable
  mergedAt
  totalCommentsCount
  comments(last: 1) {
    totalCount
    nodes { createdAt }
  }
  reviewThreads(first: 100) {
    nodes {
      comments(last: 1) {
        nodes { createdAt }
      }
    }
  }
  reviewRequests(first: 50) {
    nodes {
      requestedReviewer { ... on User { login } }
    }
  }
  latestReviews(first: 50) {
    nodes {
      author { login }
      state
    }
  }
  commits(last: 1) {
    nodes {
      commit {
        statusCheckRollup {
          contexts(first: 100) {
            nodes {
              ... on CheckRun {
                name
                status
                conclusion
              }
            }
          }
        }
      }
    }
  }
}
"#;

/// The one GitHub host that is always known.
pub const GITHUB_COM: &str = "github.com";

/// Maximum open-PR list requests made by evidence-based discovery.
const MAX_OPEN_PR_DISCOVERY_PAGES: usize = 3;

/// Maximum PRs enriched by one evidence-based discovery request.
const MAX_DISCOVERED_SESSION_PRS: usize = 20;

/// Parse a git remote URL into (host, owner, repo). Supports
/// `https://HOST/owner/repo(.git)`, `ssh://git@HOST/owner/repo`, and
/// `git@HOST:owner/repo(.git)` — the host may be github.com or a GitHub
/// Enterprise instance (whether it's one we know is the caller's problem).
pub fn parse_remote(url: &str) -> Option<(String, String, String)> {
    let (host, rest) = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        rest.split_once('/')?
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.strip_prefix("git@").unwrap_or(rest);
        rest.split_once('/')?
    } else {
        let rest = url.strip_prefix("git@")?;
        rest.split_once(':')?
    };
    // Strip an explicit port ("host:22"); hostnames have no colons.
    let host = host.split(':').next()?.trim().to_ascii_lowercase();
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = rest.split_once('/')?;
    if host.is_empty() || !host.contains('.') || owner.is_empty() || repo.is_empty() {
        return None;
    }
    if repo.contains('/') {
        return None;
    }
    Some((host, owner.to_string(), repo.to_string()))
}

/// Pull-request numbers mentioned in `text` for one repository.
///
/// Recognizes browser URLs plus public, enterprise, and relative REST API
/// paths. This is deliberately independent of the client that produced the
/// text (GitHub UI, REST, GraphQL responses, CLIs, or MCP tools).
pub fn pr_numbers_in_text(text: &str, host: &str, owner: &str, repo: &str) -> Vec<u64> {
    let text = text.to_ascii_lowercase();
    let mut numbers = Vec::new();
    let host = host.to_ascii_lowercase();
    let owner = owner.to_ascii_lowercase();
    let repo = repo.to_ascii_lowercase();
    let mut prefixes = vec![
        format!("https://{host}/{owner}/{repo}/pull/"),
        format!("http://{host}/{owner}/{repo}/pull/"),
        format!("repos/{owner}/{repo}/pulls/"),
    ];
    if host == GITHUB_COM {
        prefixes.push(format!("api.github.com/repos/{owner}/{repo}/pulls/"));
    } else {
        prefixes.push(format!("{host}/api/v3/repos/{owner}/{repo}/pulls/"));
    }
    for prefix in prefixes {
        let mut rest = text.as_str();
        while let Some(index) = rest.find(&prefix) {
            rest = &rest[index + prefix.len()..];
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            if let Ok(number) = digits.parse()
                && !numbers.contains(&number)
            {
                numbers.push(number);
            }
        }
    }
    numbers
}

/// Browser URL for a repository-local pull request number.
pub fn pr_url(host: &str, owner: &str, repo: &str, number: u64) -> String {
    format!("https://{host}/{owner}/{repo}/pull/{number}")
}

/// Whether text contains a git ref as a complete token.
fn text_mentions_ref(text: &str, reference: &str) -> bool {
    let text = text.as_bytes();
    let reference = reference.as_bytes();
    if reference.is_empty() {
        return false;
    }
    text.windows(reference.len())
        .enumerate()
        .any(|(index, part)| {
            part == reference
                && (index == 0 || !is_ref_byte(text[index - 1]))
                && (index + reference.len() == text.len()
                    || !is_ref_byte(text[index + reference.len()]))
        })
}

/// Bytes that may occur inside a git ref token.
fn is_ref_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.')
}

/// Whether a PR head matches recorded branch or commit evidence.
fn pr_head_matches_evidence(
    branch: &str,
    label: Option<&str>,
    sha: &str,
    branch_evidence: &[String],
    commit_ids: &HashSet<String>,
) -> bool {
    commit_ids.contains(&sha.to_ascii_lowercase())
        || branch_evidence.iter().any(|text| {
            text_mentions_ref(text, branch)
                || text_mentions_ref(text, &format!("refs/heads/{branch}"))
                || label.is_some_and(|label| text_mentions_ref(text, label))
        })
}

fn same_repository(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn normalized_head_label(name_with_owner: &str, branch: &str) -> Option<String> {
    name_with_owner
        .split_once('/')
        .map(|(owner, _)| format!("{}:{branch}", owner.to_ascii_lowercase()))
}

/// Token from the environment for `host`. github.com reads
/// `GITHUB_TOKEN` / `GH_TOKEN`; enterprise hosts read
/// `GH_ENTERPRISE_TOKEN` / `GITHUB_ENTERPRISE_TOKEN` (the gh CLI's own
/// convention).
/// Client id of the shared "Trouve" OAuth app on github.com, baked in so
/// sign-in works out of the box. OAuth client ids are public identifiers
/// (the device flow needs no secret); `github_client_id` in config.toml
/// overrides it. Enterprise hosts still need their own per-instance app.
pub const DEFAULT_CLIENT_ID: &str = "Ov23liEvV9xEJCsfJQ15";

/// Device-flow OAuth endpoints for a GitHub host (github.com or a GHES
/// instance — both serve the flow under /login). The client id comes from
/// config: an OAuth app on that host with device flow enabled.
pub fn oauth_config(host: &str, client_id: &str) -> trouve_providers::auth::OAuthConfig {
    trouve_providers::auth::OAuthConfig {
        client_id: client_id.to_string(),
        device_authorization_url: Some(format!("https://{host}/login/device/code")),
        authorization_url: None,
        token_url: format!("https://{host}/login/oauth/access_token"),
        // Classic OAuth-app scope covering PR read/write and checks.
        scopes: vec!["repo".into()],
        redirect_port: None,
        redirect_path: None,
    }
}

pub struct GitHub {
    client: Octocrab,
    host: String,
    graphql: GitHubGraphql,
    owner: String,
    repo: String,
}

/// A GitHub client scoped to an authenticated account rather than a repo.
pub struct GitHubAccount {
    graphql: GitHubGraphql,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlRateLimit {
    cost: u64,
    remaining: u64,
    reset_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct GraphqlViewerData {
    viewer: GraphqlActor,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
struct GraphqlActor {
    login: String,
}

#[derive(Deserialize)]
struct GraphqlSearchData {
    #[serde(default)]
    open: Option<GraphqlSearchConnection>,
    #[serde(default)]
    review: Option<GraphqlSearchConnection>,
    #[serde(default)]
    merged: Option<GraphqlSearchConnection>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlSearchConnection {
    #[serde(default)]
    nodes: Vec<Option<GraphqlNodeId>>,
    page_info: GraphqlPageInfo,
}

#[derive(Deserialize)]
struct GraphqlNodeId {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
struct GraphqlDetailsData {
    #[serde(default)]
    nodes: Vec<Option<GraphqlPullRequest>>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
struct GraphqlBranchData {
    repository: Option<GraphqlBranchRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
struct GraphqlPullRequestData {
    repository: Option<GraphqlPullRequestRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
struct GraphqlOpenPullRequestsData {
    repository: Option<GraphqlOpenPullRequestsRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlBranchRepository {
    pull_requests: GraphqlPullRequestConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPullRequestRepository {
    pull_request: Option<GraphqlPullRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlOpenPullRequestsRepository {
    pull_requests: GraphqlPagedPullRequestConnection,
}

#[derive(Deserialize)]
struct GraphqlPullRequestConnection {
    #[serde(default)]
    nodes: Vec<Option<GraphqlPullRequest>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPagedPullRequestConnection {
    #[serde(default)]
    nodes: Vec<Option<GraphqlPullRequest>>,
    page_info: GraphqlPageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPullRequest {
    repository: GraphqlRepository,
    head_repository: Option<GraphqlRepository>,
    number: u64,
    url: String,
    title: String,
    state: String,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: Option<String>,
    author: Option<GraphqlActor>,
    mergeable: String,
    merged_at: Option<DateTime<Utc>>,
    total_comments_count: Option<u64>,
    comments: GraphqlComments,
    review_threads: GraphqlReviewThreads,
    review_requests: Option<GraphqlReviewRequests>,
    latest_reviews: Option<GraphqlReviews>,
    commits: GraphqlCommits,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlRepository {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct GraphqlComments {
    #[serde(default, rename = "totalCount")]
    total_count: Option<u64>,
    #[serde(default)]
    nodes: Vec<Option<GraphqlComment>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlComment {
    created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct GraphqlReviewThreads {
    #[serde(default)]
    nodes: Vec<Option<GraphqlReviewThread>>,
}

#[derive(Deserialize)]
struct GraphqlReviewThread {
    comments: GraphqlComments,
}

#[derive(Deserialize)]
struct GraphqlReviewRequests {
    #[serde(default)]
    nodes: Vec<Option<GraphqlReviewRequest>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlReviewRequest {
    requested_reviewer: Option<GraphqlRequestedReviewer>,
}

#[derive(Deserialize)]
struct GraphqlRequestedReviewer {
    #[serde(default)]
    login: Option<String>,
}

#[derive(Deserialize)]
struct GraphqlReviews {
    #[serde(default)]
    nodes: Vec<Option<GraphqlReview>>,
}

#[derive(Deserialize)]
struct GraphqlReview {
    author: Option<GraphqlActor>,
    state: String,
}

#[derive(Deserialize)]
struct GraphqlCommits {
    #[serde(default)]
    nodes: Vec<Option<GraphqlPullRequestCommit>>,
}

#[derive(Deserialize)]
struct GraphqlPullRequestCommit {
    commit: GraphqlCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlCommit {
    status_check_rollup: Option<GraphqlStatusCheckRollup>,
}

#[derive(Deserialize)]
struct GraphqlStatusCheckRollup {
    contexts: GraphqlCheckContexts,
}

#[derive(Deserialize)]
struct GraphqlCheckContexts {
    #[serde(default)]
    nodes: Vec<Option<GraphqlCheckRun>>,
}

#[derive(Deserialize)]
struct GraphqlCheckRun {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
}

struct SearchCursor {
    query: String,
    after: Option<String>,
    pages: usize,
    active: bool,
}

impl SearchCursor {
    fn new(query: String) -> Self {
        Self {
            query,
            after: None,
            pages: 0,
            active: true,
        }
    }
}

struct GitHubGraphql {
    client: Octocrab,
    host: String,
}

impl GitHubGraphql {
    fn new(token: &str, host: &str) -> Result<Self> {
        let mut builder = Octocrab::builder().personal_token(token.to_string());
        if let Some(base) = graphql_base_uri(host) {
            builder = builder
                .base_uri(base)
                .context("enterprise GraphQL base URI")?;
        }
        Ok(Self {
            client: builder.build().context("building GitHub GraphQL client")?,
            host: host.into(),
        })
    }

    async fn viewer(&self) -> Result<String> {
        let response: GraphqlViewerData = self
            .client
            .graphql(&serde_json::json!({ "query": VIEWER_QUERY }))
            .await
            .context("looking up GitHub viewer through GraphQL")?;
        self.trace_rate("viewer", &response.rate_limit);
        Ok(response.viewer.login)
    }

    async fn dashboard_prs(&self, merged_since: DateTime<Utc>) -> Result<(String, Vec<PrInfo>)> {
        let viewer = self.viewer().await?;
        let day = merged_since.format("%Y-%m-%d");
        let mut open = SearchCursor::new(format!("is:pr is:open involves:{viewer}"));
        let mut review = SearchCursor::new(format!("is:pr is:open review-requested:{viewer}"));
        let mut merged =
            SearchCursor::new(format!("is:pr is:merged merged:>={day} involves:{viewer}"));
        let mut ids = std::collections::BTreeSet::new();

        while open.active || review.active || merged.active {
            let response: GraphqlSearchData = self
                .client
                .graphql(&serde_json::json!({
                    "query": DASHBOARD_SEARCH_QUERY,
                    "variables": {
                        "openQuery": open.query,
                        "reviewQuery": review.query,
                        "mergedQuery": merged.query,
                        "openAfter": open.after,
                        "reviewAfter": review.after,
                        "mergedAfter": merged.after,
                        "includeOpen": open.active,
                        "includeReview": review.active,
                        "includeMerged": merged.active,
                    }
                }))
                .await
                .context("searching account pull requests through GraphQL")?;
            self.trace_rate("pull request search", &response.rate_limit);
            consume_search_page(response.open, &mut open, &mut ids);
            consume_search_page(response.review, &mut review, &mut ids);
            consume_search_page(response.merged, &mut merged, &mut ids);
        }

        let mut prs = self.pull_requests_by_id(ids.into_iter().collect()).await?;
        prs.sort_by_key(|pr| std::cmp::Reverse(pr.number));
        Ok((viewer, prs))
    }

    async fn pull_requests_by_id(&self, ids: Vec<String>) -> Result<Vec<PrInfo>> {
        let mut prs = Vec::with_capacity(ids.len());
        let query = operation_with_pr_fields(DASHBOARD_DETAILS_QUERY);
        for ids in ids.chunks(DASHBOARD_GRAPHQL_BATCH) {
            let response: GraphqlDetailsData = self
                .client
                .graphql(&serde_json::json!({
                    "query": query,
                    "variables": { "ids": ids }
                }))
                .await
                .context("loading pull request details through GraphQL")?;
            self.trace_rate("pull request details", &response.rate_limit);
            prs.extend(
                response
                    .nodes
                    .into_iter()
                    .flatten()
                    .map(|pr| pr.into_pr_info(&self.host)),
            );
        }
        Ok(prs)
    }

    async fn branch_prs(
        &self,
        owner: &str,
        repository: &str,
        branch: &str,
        open_only: bool,
    ) -> Result<Vec<PrInfo>> {
        let states = if open_only {
            vec!["OPEN"]
        } else {
            vec!["OPEN", "CLOSED", "MERGED"]
        };
        let query = operation_with_pr_fields(BRANCH_PULL_REQUESTS_QUERY);
        let response: GraphqlBranchData = self
            .client
            .graphql(&serde_json::json!({
                "query": query,
                "variables": {
                    "owner": owner,
                    "repository": repository,
                    "branch": branch,
                    "states": states,
                }
            }))
            .await
            .context("listing branch pull requests through GraphQL")?;
        self.trace_rate("branch pull requests", &response.rate_limit);
        let head_repository = format!("{owner}/{repository}");
        let mut prs: Vec<_> = response
            .repository
            .into_iter()
            .flat_map(|repository| repository.pull_requests.nodes)
            .flatten()
            .filter(|pr| {
                pr.head_repository.as_ref().is_some_and(|repository| {
                    same_repository(&repository.name_with_owner, &head_repository)
                })
            })
            .map(|pr| pr.into_pr_info(&self.host))
            .collect();
        prs.sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
        Ok(prs)
    }

    async fn pull_request(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
    ) -> Result<Option<PrInfo>> {
        let query = operation_with_pr_fields(PULL_REQUEST_QUERY);
        let response: GraphqlPullRequestData = self
            .client
            .graphql(&serde_json::json!({
                "query": query,
                "variables": {
                    "owner": owner,
                    "repository": repository,
                    "number": number,
                }
            }))
            .await
            .context("loading pull request through GraphQL")?;
        self.trace_rate("pull request", &response.rate_limit);
        Ok(response
            .repository
            .and_then(|repository| repository.pull_request)
            .map(|pr| pr.into_pr_info(&self.host)))
    }

    async fn open_prs_referenced_by(
        &self,
        owner: &str,
        repository: &str,
        branch_evidence: &[String],
        commit_ids: &HashSet<String>,
    ) -> Result<Vec<PrInfo>> {
        if branch_evidence.is_empty() && commit_ids.is_empty() {
            return Ok(Vec::new());
        }

        let query = operation_with_pr_fields(OPEN_PULL_REQUESTS_QUERY);
        let mut after = None;
        let mut prs = Vec::new();
        for _ in 0..MAX_OPEN_PR_DISCOVERY_PAGES {
            let response: GraphqlOpenPullRequestsData = self
                .client
                .graphql(&serde_json::json!({
                    "query": query,
                    "variables": {
                        "owner": owner,
                        "repository": repository,
                        "after": after,
                    }
                }))
                .await
                .context("listing open pull requests through GraphQL")?;
            self.trace_rate("open pull request discovery", &response.rate_limit);
            let Some(repository) = response.repository else {
                break;
            };
            let page = repository.pull_requests;
            for pr in page.nodes.into_iter().flatten() {
                let label = pr.head_repository.as_ref().and_then(|repository| {
                    normalized_head_label(&repository.name_with_owner, &pr.head_ref_name)
                });
                if pr_head_matches_evidence(
                    &pr.head_ref_name,
                    label.as_deref(),
                    pr.head_ref_oid.as_deref().unwrap_or_default(),
                    branch_evidence,
                    commit_ids,
                ) {
                    prs.push(pr.into_pr_info(&self.host));
                    if prs.len() == MAX_DISCOVERED_SESSION_PRS {
                        return Ok(prs);
                    }
                }
            }
            if !page.page_info.has_next_page {
                break;
            }
            let Some(cursor) = page.page_info.end_cursor else {
                break;
            };
            after = Some(cursor);
        }
        Ok(prs)
    }

    fn trace_rate(&self, operation: &str, rate: &GraphqlRateLimit) {
        tracing::debug!(
            host = self.host,
            operation,
            cost = rate.cost,
            remaining = rate.remaining,
            reset_at = %rate.reset_at,
            "GitHub GraphQL request"
        );
    }
}

impl GraphqlPullRequest {
    fn into_pr_info(self, host: &str) -> PrInfo {
        let comments = self
            .total_comments_count
            .or(self.comments.total_count)
            .unwrap_or_default();
        let issue_comment_at = newest_comment(self.comments);
        let review_comment_at = self
            .review_threads
            .nodes
            .into_iter()
            .flatten()
            .filter_map(|thread| newest_comment(thread.comments))
            .max();
        let last_comment_at = match (issue_comment_at, review_comment_at) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let requested_reviewers = self
            .review_requests
            .into_iter()
            .flat_map(|requests| requests.nodes)
            .flatten()
            .filter_map(|request| request.requested_reviewer?.login)
            .collect();
        let reviews = self
            .latest_reviews
            .into_iter()
            .flat_map(|reviews| reviews.nodes)
            .flatten()
            .filter_map(|review| {
                Some(PrReview {
                    reviewer: review.author?.login,
                    state: review.state.to_ascii_lowercase(),
                })
            })
            .collect();
        let checks = self
            .commits
            .nodes
            .into_iter()
            .flatten()
            .filter_map(|pull_request_commit| pull_request_commit.commit.status_check_rollup)
            .flat_map(|rollup| rollup.contexts.nodes)
            .flatten()
            .filter_map(|check| {
                Some(CheckRun {
                    name: check.name?,
                    status: check.status?.to_ascii_lowercase(),
                    conclusion: check.conclusion.map(|value| value.to_ascii_lowercase()),
                })
            })
            .collect();

        PrInfo {
            host: host.into(),
            repository: self.repository.name_with_owner,
            workspace_id: String::new(),
            number: self.number,
            url: self.url,
            title: self.title,
            state: self.state.to_ascii_lowercase(),
            draft: self.is_draft,
            base: self.base_ref_name,
            head: self.head_ref_name,
            checks,
            reviews,
            author: self.author.map(|author| author.login).unwrap_or_default(),
            requested_reviewers,
            comments,
            last_comment_at,
            mergeable: match self.mergeable.as_str() {
                "MERGEABLE" => Some(true),
                "CONFLICTING" => Some(false),
                _ => None,
            },
            merged_at: self.merged_at,
        }
    }
}

fn newest_comment(comments: GraphqlComments) -> Option<DateTime<Utc>> {
    comments
        .nodes
        .into_iter()
        .flatten()
        .map(|comment| comment.created_at)
        .max()
}

fn consume_search_page(
    page: Option<GraphqlSearchConnection>,
    cursor: &mut SearchCursor,
    ids: &mut std::collections::BTreeSet<String>,
) {
    if !cursor.active {
        return;
    }
    let Some(page) = page else {
        cursor.active = false;
        return;
    };
    ids.extend(page.nodes.into_iter().flatten().map(|node| node.id));
    cursor.pages += 1;
    cursor.active = page.page_info.has_next_page
        && cursor.pages < DASHBOARD_MAX_PR_PAGES
        && page.page_info.end_cursor.is_some();
    cursor.after = cursor.active.then_some(page.page_info.end_cursor).flatten();
}

fn operation_with_pr_fields(operation: &str) -> String {
    format!("{operation}\n{PULL_REQUEST_FIELDS}")
}

fn graphql_base_uri(host: &str) -> Option<String> {
    (host != GITHUB_COM).then(|| format!("https://{host}/api"))
}

impl GitHubAccount {
    pub fn new(token: &str, host: &str) -> Result<Self> {
        Ok(Self {
            graphql: GitHubGraphql::new(token, host)?,
        })
    }

    pub async fn dashboard_prs(
        &self,
        merged_since: DateTime<Utc>,
    ) -> Result<(String, Vec<PrInfo>)> {
        self.graphql.dashboard_prs(merged_since).await
    }
}

impl GitHub {
    pub fn new(token: &str, host: &str, owner: &str, repo: &str) -> Result<Self> {
        let mut builder = Octocrab::builder().personal_token(token.to_string());
        if host != GITHUB_COM {
            // GitHub Enterprise Server exposes the REST API under /api/v3.
            builder = builder
                .base_uri(format!("https://{host}/api/v3"))
                .context("enterprise API base URI")?;
        }
        let client = builder.build().context("building GitHub client")?;
        Ok(Self {
            client,
            host: host.to_string(),
            graphql: GitHubGraphql::new(token, host)?,
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    /// The open PR whose head is `branch`, if any.
    pub async fn pr_for_branch(&self, branch: &str) -> Result<Option<PrInfo>> {
        Ok(self
            .graphql
            .branch_prs(&self.owner, &self.repo, branch, true)
            .await?
            .into_iter()
            .next())
    }

    /// A PR by repository-local number, regardless of its head branch.
    pub async fn pr(&self, number: u64) -> Result<PrInfo> {
        self.graphql
            .pull_request(&self.owner, &self.repo, number)
            .await?
            .with_context(|| format!("pull request #{number} not found"))
    }

    /// Open PRs whose head ref or commit is tied to successful activity in a
    /// session. This discovers PRs opened later through the GitHub UI, REST,
    /// GraphQL, or another client after the session created or pushed them.
    pub async fn open_prs_referenced_by(
        &self,
        branch_evidence: &[String],
        commit_ids: &HashSet<String>,
    ) -> Result<Vec<PrInfo>> {
        self.graphql
            .open_prs_referenced_by(&self.owner, &self.repo, branch_evidence, commit_ids)
            .await
    }

    /// Every PR (open, merged, or closed) whose head is `branch`, open ones
    /// first, newest first within each group.
    pub async fn prs_for_branch(&self, branch: &str) -> Result<Vec<PrInfo>> {
        self.graphql
            .branch_prs(&self.owner, &self.repo, branch, false)
            .await
    }

    pub async fn create_pr(
        &self,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<PrInfo> {
        let pr = self
            .client
            .pulls(&self.owner, &self.repo)
            .create(title, branch, base)
            .body(body)
            .draft(Some(draft))
            .send()
            .await
            .context("creating PR")?;
        self.enrich(pr).await
    }

    /// Login of the authenticated user (whose token this client holds).
    pub async fn viewer(&self) -> Result<String> {
        self.graphql.viewer().await
    }

    pub async fn merge_pr(&self, number: u64, method: &str) -> Result<()> {
        let method = match method {
            "squash" => octocrab::params::pulls::MergeMethod::Squash,
            "rebase" => octocrab::params::pulls::MergeMethod::Rebase,
            _ => octocrab::params::pulls::MergeMethod::Merge,
        };
        let result = self
            .client
            .pulls(&self.owner, &self.repo)
            .merge(number)
            .method(method)
            .send()
            .await
            .context("merging PR")?;
        if !result.merged {
            anyhow::bail!(
                "merge refused: {}",
                result.message.unwrap_or_else(|| "unknown reason".into())
            );
        }
        Ok(())
    }

    /// Attach checks and reviews to the raw PR model.
    async fn enrich(&self, pr: octocrab::models::pulls::PullRequest) -> Result<PrInfo> {
        let head_sha = pr.head.sha.clone();
        let number = pr.number;

        let checks = self
            .client
            .checks(&self.owner, &self.repo)
            .list_check_runs_for_git_ref(octocrab::params::repos::Commitish(head_sha))
            .send()
            .await
            .map(|runs| {
                runs.check_runs
                    .into_iter()
                    .map(|run| CheckRun {
                        name: run.name,
                        // octocrab's CheckRun has no status field; derive it
                        // from completion timestamps.
                        status: if run.completed_at.is_some() {
                            "completed".to_string()
                        } else if run.started_at.is_some() {
                            "in_progress".to_string()
                        } else {
                            "queued".to_string()
                        },
                        conclusion: run.conclusion,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let reviews = self
            .client
            .pulls(&self.owner, &self.repo)
            .list_reviews(number)
            .per_page(50)
            .send()
            .await
            .map(|page| {
                page.items
                    .into_iter()
                    .map(|review| PrReview {
                        reviewer: review.user.map(|u| u.login).unwrap_or_default(),
                        state: review
                            .state
                            .map(|s| format!("{s:?}").to_lowercase())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(PrInfo {
            host: self.host.clone(),
            repository: format!("{}/{}", self.owner, self.repo),
            workspace_id: String::new(),
            number,
            url: pr.html_url.map(|u| u.to_string()).unwrap_or_default(),
            title: pr.title.unwrap_or_default(),
            // GitHub reports merged PRs as "closed"; distinguish them.
            state: if pr.merged_at.is_some() {
                "merged".to_string()
            } else {
                pr.state
                    .map(|s| format!("{s:?}").to_lowercase())
                    .unwrap_or_default()
            },
            draft: pr.draft.unwrap_or(false),
            base: pr.base.ref_field,
            head: pr.head.ref_field,
            checks,
            reviews,
            author: pr.user.map(|u| u.login).unwrap_or_default(),
            requested_reviewers: pr
                .requested_reviewers
                .unwrap_or_default()
                .into_iter()
                .map(|u| u.login)
                .collect(),
            // Comment info comes from the dashboard path only — the extra
            // requests aren't worth it for per-session lookups.
            comments: 0,
            last_comment_at: None,
            // Populated on list responses only via the dashboard's
            // single-PR GET; present here when `pr` came from one.
            mergeable: pr.mergeable,
            merged_at: pr.merged_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_forms() {
        for url in [
            "https://github.com/jimsimon/trouve.git",
            "https://github.com/jimsimon/trouve",
            "git@github.com:jimsimon/trouve.git",
            "ssh://git@github.com/jimsimon/trouve",
        ] {
            assert_eq!(
                parse_remote(url),
                Some(("github.com".into(), "jimsimon".into(), "trouve".into())),
                "{url}"
            );
        }
        // Enterprise hosts parse the same way; whether they're configured
        // is the engine's call.
        for url in [
            "https://GitHub.Example.com/team/tool.git",
            "git@github.example.com:team/tool",
            "ssh://git@github.example.com/team/tool.git",
        ] {
            assert_eq!(
                parse_remote(url),
                Some(("github.example.com".into(), "team".into(), "tool".into())),
                "{url}"
            );
        }
        assert_eq!(
            parse_remote("https://gitlab.com/x/y.git"),
            Some(("gitlab.com".into(), "x".into(), "y".into()))
        );
        assert_eq!(parse_remote("git@github.com:broken"), None);
        assert_eq!(parse_remote("/local/path/repo.git"), None);
    }

    #[test]
    fn finds_only_pr_urls_for_the_expected_repository() {
        let text = concat!(
            "created https://github.com/JimSimon/Trouve/pull/73, ",
            "then viewed https://github.com/jimsimon/trouve/pull/73#discussion; ",
            "REST https://api.github.com/repos/jimsimon/trouve/pulls/74; ",
            "relative repos/jimsimon/trouve/pulls/75/comments; ",
            "ignore https://github.com/other/repo/pull/9"
        );
        assert_eq!(
            pr_numbers_in_text(text, "github.com", "jimsimon", "trouve"),
            vec![73, 74, 75]
        );
        assert!(pr_numbers_in_text(text, "github.com", "other", "project").is_empty());
    }

    #[test]
    fn matches_head_refs_on_token_boundaries() {
        assert!(text_mentions_ref(
            "git push origin fix/cross-branch-pr",
            "fix/cross-branch-pr"
        ));
        assert!(text_mentions_ref(
            r#"{\"head\":\"alice:fix/cross-branch-pr\"}"#,
            "alice:fix/cross-branch-pr"
        ));
        assert!(pr_head_matches_evidence(
            "fix/cross-branch-pr",
            None,
            "unrelated",
            &[r#"{\"ref\":\"refs/heads/fix/cross-branch-pr\"}"#.into()],
            &HashSet::new(),
        ));
        assert!(!text_mentions_ref(
            "git push origin prefix-fix/cross-branch-pr-old",
            "fix/cross-branch-pr"
        ));
        assert!(pr_head_matches_evidence(
            "unmentioned-branch",
            None,
            "9F2C6D8B18C86D48CA2C3F58191F9F5277B9269A",
            &[],
            &HashSet::from(["9f2c6d8b18c86d48ca2c3f58191f9f5277b9269a".into()]),
        ));
    }

    #[test]
    fn repository_matching_and_head_labels_normalize_owner_case() {
        assert!(same_repository("JimSimon/Trouve", "jimsimon/trouve"));
        assert_eq!(
            normalized_head_label("JimSimon/Trouve", "fix/graphql-refresh"),
            Some("jimsimon:fix/graphql-refresh".into())
        );
    }

    #[test]
    fn enterprise_graphql_uses_the_api_graphql_base() {
        assert_eq!(graphql_base_uri(GITHUB_COM), None);
        assert_eq!(
            graphql_base_uri("github.example.com"),
            Some("https://github.example.com/api".into())
        );
    }

    #[test]
    fn graphql_pull_request_maps_dashboard_fields() {
        let raw = serde_json::json!({
            "repository": { "nameWithOwner": "acme/widgets" },
            "headRepository": { "nameWithOwner": "acme/widgets" },
            "number": 42,
            "url": "https://github.example.com/acme/widgets/pull/42",
            "title": "Ship the widgets",
            "state": "OPEN",
            "isDraft": false,
            "baseRefName": "main",
            "headRefName": "ship-widgets",
            "author": { "login": "alice" },
            "mergeable": "CONFLICTING",
            "mergedAt": null,
            "totalCommentsCount": 4,
            "comments": {
                "totalCount": 2,
                "nodes": [{ "createdAt": "2026-07-20T10:00:00Z" }]
            },
            "reviewThreads": {
                "nodes": [{
                    "comments": {
                        "nodes": [{ "createdAt": "2026-07-20T11:00:00Z" }]
                    }
                }]
            },
            "reviewRequests": {
                "nodes": [
                    { "requestedReviewer": { "login": "bob" } },
                    { "requestedReviewer": {} }
                ]
            },
            "latestReviews": {
                "nodes": [
                    { "author": { "login": "carol" }, "state": "CHANGES_REQUESTED" }
                ]
            },
            "commits": {
                "nodes": [{
                    "commit": {
                        "statusCheckRollup": {
                            "contexts": {
                                "nodes": [
                                    {
                                        "name": "test",
                                        "status": "COMPLETED",
                                        "conclusion": "SUCCESS"
                                    },
                                    {}
                                ]
                            }
                        }
                    }
                }]
            }
        });
        let pr: GraphqlPullRequest = serde_json::from_value(raw).unwrap();
        let info = pr.into_pr_info("github.example.com");

        assert_eq!(info.host, "github.example.com");
        assert_eq!(info.repository, "acme/widgets");
        assert_eq!(info.number, 42);
        assert_eq!(info.state, "open");
        assert_eq!(info.author, "alice");
        assert_eq!(info.requested_reviewers, ["bob"]);
        assert_eq!(info.comments, 4);
        assert_eq!(
            info.last_comment_at,
            Some("2026-07-20T11:00:00Z".parse().unwrap())
        );
        assert_eq!(info.mergeable, Some(false));
        assert_eq!(info.reviews.len(), 1);
        assert_eq!(info.reviews[0].reviewer, "carol");
        assert_eq!(info.reviews[0].state, "changes_requested");
        assert_eq!(info.checks.len(), 1);
        assert_eq!(info.checks[0].name, "test");
        assert_eq!(info.checks[0].status, "completed");
        assert_eq!(info.checks[0].conclusion.as_deref(), Some("success"));
    }

    #[test]
    fn graphql_search_stops_at_the_dashboard_page_cap() {
        let mut cursor = SearchCursor::new("is:pr".into());
        let mut ids = std::collections::BTreeSet::new();
        for page in 1..=DASHBOARD_MAX_PR_PAGES {
            consume_search_page(
                Some(GraphqlSearchConnection {
                    nodes: vec![Some(GraphqlNodeId {
                        id: format!("pr-{page}"),
                    })],
                    page_info: GraphqlPageInfo {
                        has_next_page: true,
                        end_cursor: Some(format!("cursor-{page}")),
                    },
                }),
                &mut cursor,
                &mut ids,
            );
        }

        assert_eq!(ids.len(), DASHBOARD_MAX_PR_PAGES);
        assert!(!cursor.active);
        assert_eq!(cursor.pages, DASHBOARD_MAX_PR_PAGES);
    }
}
