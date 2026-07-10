//! GitHub PR integration (Phase 5 slice): create, inspect, and merge the PR
//! for a session branch via octocrab.
//!
//! Covered today: PR lookup by branch, create (incl. draft), combined
//! status (checks + reviews), merge with method selection. Review threads,
//! merge queues (GraphQL), and GitLab are tracked as follow-ups in the
//! plan.

use anyhow::{Context, Result};
use octocrab::Octocrab;
use trouve_protocol::{CheckRun, PrInfo, PrReview};

/// Parse a git remote URL into (owner, repo). Supports
/// `https://github.com/owner/repo(.git)` and `git@github.com:owner/repo(.git)`.
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// GitHub token: `GITHUB_TOKEN` / `GH_TOKEN` env vars.
pub fn token_from_env() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .ok()
        .filter(|t| !t.is_empty())
}

pub struct GitHub {
    client: Octocrab,
    owner: String,
    repo: String,
}

impl GitHub {
    pub fn new(token: &str, owner: &str, repo: &str) -> Result<Self> {
        let client = Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .context("building GitHub client")?;
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
                parse_github_remote(url),
                Some(("jimsimon".into(), "trouve".into())),
                "{url}"
            );
        }
        assert_eq!(parse_github_remote("https://gitlab.com/x/y.git"), None);
        assert_eq!(parse_github_remote("git@github.com:broken"), None);
    }
}
