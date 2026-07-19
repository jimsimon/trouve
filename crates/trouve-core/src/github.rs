//! GitHub PR integration (Phase 5 slice): create, inspect, and merge PRs
//! associated with a session via octocrab.
//!
//! Covered today: PR lookup by branch and session activity, create (incl.
//! draft), combined status (checks + reviews), and merge with method
//! selection. Review threads, merge queues (GraphQL), and GitLab are tracked
//! as follow-ups in the plan.

use std::collections::HashSet;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use octocrab::Octocrab;
use serde::Deserialize;
use trouve_protocol::{CheckRun, PrInfo, PrReview};

/// A dashboard refresh should stay bounded even for repositories with an
/// unusually deep PR history. Each page contains up to 100 PRs.
const DASHBOARD_MAX_PR_PAGES: usize = 3;
const DASHBOARD_ENRICH_CONCURRENCY: usize = 4;

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
                || label.is_some_and(|label| text_mentions_ref(text, label))
        })
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
    owner: String,
    repo: String,
}

/// A GitHub client scoped to an authenticated account rather than a repo.
pub struct GitHubAccount {
    client: Octocrab,
    host: String,
}

#[derive(Deserialize)]
struct SearchIssues {
    items: Vec<SearchIssue>,
}

#[derive(Deserialize)]
struct SearchIssue {
    number: u64,
    repository_url: String,
}

impl GitHubAccount {
    pub fn new(token: &str, host: &str) -> Result<Self> {
        let mut builder = Octocrab::builder().personal_token(token.to_string());
        if host != GITHUB_COM {
            builder = builder
                .base_uri(format!("https://{host}/api/v3"))
                .context("enterprise API base URI")?;
        }
        Ok(Self {
            client: builder.build().context("building GitHub client")?,
            host: host.into(),
        })
    }

    pub async fn dashboard_prs(
        &self,
        merged_since: DateTime<Utc>,
    ) -> Result<(String, Vec<PrInfo>)> {
        let viewer = self
            .client
            .current()
            .user()
            .await
            .context("looking up GitHub viewer")?
            .login;
        let day = merged_since.format("%Y-%m-%d");
        let queries = [
            format!("is:pr is:open involves:{viewer}"),
            format!("is:pr is:open review-requested:{viewer}"),
            format!("is:pr is:merged merged:>={day} involves:{viewer}"),
        ];
        let mut refs = std::collections::BTreeSet::new();
        for query in queries {
            for page_number in 1..=DASHBOARD_MAX_PR_PAGES {
                let page: SearchIssues = self
                    .client
                    .get(
                        "/search/issues",
                        Some(&serde_json::json!({
                            "q": query, "per_page": 100, "page": page_number
                        })),
                    )
                    .await
                    .context("searching account pull requests")?;
                let count = page.items.len();
                for issue in page.items {
                    let Some(repo) = issue.repository_url.rsplit_once("/repos/").map(|(_, r)| r)
                    else {
                        continue;
                    };
                    if let Some((owner, name)) = repo.split_once('/') {
                        refs.insert((owner.to_string(), name.to_string(), issue.number));
                    }
                }
                if count < 100 {
                    break;
                }
            }
        }
        let host = self.host.clone();
        let token_client = self.client.clone();
        let mut prs: Vec<PrInfo> = futures::stream::iter(refs)
            .map(|(owner, repo, number)| {
                let client = token_client.clone();
                let host = host.clone();
                async move {
                    let raw = client.pulls(&owner, &repo).get(number).await?;
                    let issue_comments = raw.comments.unwrap_or(0);
                    let review_comments = raw.review_comments.unwrap_or(0);
                    let mergeable = raw.mergeable;
                    let github = GitHub {
                        client,
                        owner,
                        repo,
                    };
                    let mut info = github.enrich(raw).await?;
                    github
                        .attach_comment_info(
                            number,
                            issue_comments,
                            review_comments,
                            mergeable,
                            &mut info,
                        )
                        .await;
                    info.host = host;
                    Ok::<_, anyhow::Error>(info)
                }
            })
            .buffer_unordered(DASHBOARD_ENRICH_CONCURRENCY)
            .try_collect()
            .await?;
        prs.sort_by_key(|pr| std::cmp::Reverse(pr.number));
        Ok((viewer, prs))
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
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    /// The open PR whose head is `branch`, if any.
    pub async fn pr_for_branch(&self, branch: &str) -> Result<Option<PrInfo>> {
        let page = self
            .client
            .pulls(&self.owner, &self.repo)
            .list()
            .head(format!("{}:{branch}", self.owner))
            .per_page(1)
            .send()
            .await
            .context("listing PRs")?;
        match page.items.into_iter().next() {
            Some(pr) => Ok(Some(self.enrich(pr).await?)),
            None => Ok(None),
        }
    }

    /// A PR by repository-local number, regardless of its head branch.
    pub async fn pr(&self, number: u64) -> Result<PrInfo> {
        let pr = self
            .client
            .pulls(&self.owner, &self.repo)
            .get(number)
            .await
            .context("getting PR")?;
        self.enrich(pr).await
    }

    /// Open PRs whose head ref or commit is tied to successful activity in a
    /// session. This discovers PRs opened later through the GitHub UI, REST,
    /// GraphQL, or another client after the session created or pushed them.
    pub async fn open_prs_referenced_by(
        &self,
        branch_evidence: &[String],
        commit_ids: &HashSet<String>,
    ) -> Result<Vec<PrInfo>> {
        if branch_evidence.is_empty() && commit_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut page = self
            .client
            .pulls(&self.owner, &self.repo)
            .list()
            .per_page(100)
            .send()
            .await
            .context("listing open PRs")?;
        let mut prs = Vec::new();
        for page_number in 0..MAX_OPEN_PR_DISCOVERY_PAGES {
            for pr in page.take_items() {
                let branch = &pr.head.ref_field;
                let label = pr.head.label.as_deref();
                if pr_head_matches_evidence(
                    branch,
                    label,
                    &pr.head.sha,
                    branch_evidence,
                    commit_ids,
                ) {
                    prs.push(self.enrich(pr).await?);
                    if prs.len() == MAX_DISCOVERED_SESSION_PRS {
                        return Ok(prs);
                    }
                }
            }
            if page_number + 1 == MAX_OPEN_PR_DISCOVERY_PAGES {
                break;
            }
            let Some(next) = self
                .client
                .get_page(&page.next)
                .await
                .context("listing open PR pages")?
            else {
                break;
            };
            page = next;
        }
        Ok(prs)
    }

    /// Every PR (open, merged, or closed) whose head is `branch`, open ones
    /// first, newest first within each group.
    pub async fn prs_for_branch(&self, branch: &str) -> Result<Vec<PrInfo>> {
        let page = self
            .client
            .pulls(&self.owner, &self.repo)
            .list()
            .state(octocrab::params::State::All)
            .head(format!("{}:{branch}", self.owner))
            .per_page(20)
            .send()
            .await
            .context("listing PRs")?;
        let mut prs = Vec::new();
        for pr in page.items {
            prs.push(self.enrich(pr).await?);
        }
        prs.sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
        Ok(prs)
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
        Ok(self.client.current().user().await?.login)
    }

    /// Dashboard listing: every open PR plus PRs merged since `merged_since`,
    /// each enriched with checks, reviews, and comment info.
    pub async fn dashboard_prs(&self, merged_since: DateTime<Utc>) -> Result<Vec<PrInfo>> {
        let mut open_page = self
            .client
            .pulls(&self.owner, &self.repo)
            .list()
            .state(octocrab::params::State::Open)
            .per_page(100)
            .send()
            .await
            .context("listing open PRs")?;
        let mut open = open_page.take_items();
        for _ in 1..DASHBOARD_MAX_PR_PAGES {
            let Some(mut page) = self
                .client
                .get_page::<octocrab::models::pulls::PullRequest>(&open_page.next)
                .await
                .context("listing additional open PRs")?
            else {
                break;
            };
            open.append(&mut page.items);
            open_page = page;
        }

        // Recently merged PRs hide among the closed ones; recently *updated*
        // closed PRs are a superset of recently merged. Walk pages sorted by
        // update time until they cross the merge window instead of silently
        // truncating busy repositories at one page.
        let mut closed_page = self
            .client
            .pulls(&self.owner, &self.repo)
            .list()
            .state(octocrab::params::State::Closed)
            .sort(octocrab::params::pulls::Sort::Updated)
            .direction(octocrab::params::Direction::Descending)
            .per_page(100)
            .send()
            .await
            .context("listing closed PRs")?;
        let mut recently_merged = Vec::new();
        for page_index in 0..DASHBOARD_MAX_PR_PAGES {
            let reached_cutoff = closed_page
                .items
                .iter()
                .filter_map(|pr| pr.updated_at)
                .next_back()
                .is_some_and(|updated| updated < merged_since);
            let next = closed_page.next.clone();
            recently_merged.extend(
                closed_page
                    .items
                    .into_iter()
                    .filter(|pr| pr.merged_at.is_some_and(|at| at >= merged_since)),
            );
            if reached_cutoff || page_index + 1 == DASHBOARD_MAX_PR_PAGES {
                break;
            }
            let Some(page) = self
                .client
                .get_page::<octocrab::models::pulls::PullRequest>(&next)
                .await
                .context("listing recently updated closed PRs")?
            else {
                break;
            };
            closed_page = page;
        }

        // A dashboard may span several active repositories and enrichment
        // needs multiple GitHub endpoints per PR. A small concurrency cap
        // keeps refreshes responsive without turning a busy repo into an API
        // burst.
        futures::stream::iter(open.into_iter().chain(recently_merged))
            .map(|pr| async move {
                let number = pr.number;
                // List responses omit comment counts and mergeability. Fetch
                // the full PR once, then reuse those fields throughout
                // enrichment instead of fetching it again afterward.
                let raw = self
                    .client
                    .pulls(&self.owner, &self.repo)
                    .get(number)
                    .await
                    .unwrap_or(pr);
                let issue_comments = raw.comments.unwrap_or(0);
                let review_comments = raw.review_comments.unwrap_or(0);
                let mergeable = raw.mergeable;
                let mut info = self.enrich(raw).await?;
                self.attach_comment_info(
                    number,
                    issue_comments,
                    review_comments,
                    mergeable,
                    &mut info,
                )
                .await;
                Ok::<_, anyhow::Error>(info)
            })
            .buffer_unordered(DASHBOARD_ENRICH_CONCURRENCY)
            .try_collect()
            .await
    }

    /// Fill comment totals, mergeability, and the newest comment timestamp
    /// from fields already obtained with the full PR. The two comment-list
    /// calls are best-effort and only needed to discover timestamps.
    async fn attach_comment_info(
        &self,
        number: u64,
        issue_comments: u64,
        review_comments: u64,
        mergeable: Option<bool>,
        info: &mut PrInfo,
    ) {
        info.mergeable = mergeable;
        info.comments = issue_comments + review_comments;

        let mut last: Option<DateTime<Utc>> = None;
        if issue_comments > 0 {
            // Issue comments list oldest-first with no sort option; one
            // comment per page makes page N the Nth (so last) comment.
            let newest = self
                .client
                .issues(&self.owner, &self.repo)
                .list_comments(number)
                .per_page(1)
                .page(u32::try_from(issue_comments).unwrap_or(u32::MAX))
                .send()
                .await;
            if let Ok(page) = newest {
                last = page.items.first().map(|c| c.created_at);
            }
        }
        if review_comments > 0 {
            let newest = self
                .client
                .pulls(&self.owner, &self.repo)
                .list_comments(Some(number))
                .sort(octocrab::params::pulls::comments::Sort::Created)
                .direction(octocrab::params::Direction::Descending)
                .per_page(1)
                .send()
                .await;
            if let Ok(page) = newest {
                let newest = page.items.first().map(|c| c.created_at);
                last = match (last, newest) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            }
        }
        info.last_comment_at = last;
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
            host: String::new(),
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
}
