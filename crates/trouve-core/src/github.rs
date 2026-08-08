//! GitHub PR integration: account and repository reads use batched GraphQL;
//! create and selected-PR collaboration mutations use GitHub's public REST
//! and GraphQL APIs.
//!
//! Covered today: account-wide dashboard discovery, PR lookup by branch and
//! session activity, create (incl. draft), combined status (checks + reviews),
//! and the selected PR's conversation, review, metadata, merge, merge-queue,
//! auto-merge, and native stack state. GitLab remains a follow-up.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use octocrab::Octocrab;
use serde::Deserialize;
use trouve_protocol::{
    CheckRun, GithubPrList, PrActionRequest, PrActor, PrAutoMerge, PrCapabilities, PrComment,
    PrCommentKind, PrCommit, PrDetail, PrDetailSection, PrFile, PrFileDiff, PrInfo, PrLabel,
    PrMergeQueueEntry, PrMergeQueueStatus, PrMilestone, PrReactionSummary, PrReview,
    PrReviewDetail, PrReviewThread, PrStack, PrStackEntry,
};

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
    nodes { ... on PullRequest { ...TrouvePullRequestProbe } }
    pageInfo { hasNextPage endCursor }
  }
  review: search(query: $reviewQuery, type: ISSUE, first: 100, after: $reviewAfter)
    @include(if: $includeReview) {
    nodes { ... on PullRequest { ...TrouvePullRequestProbe } }
    pageInfo { hasNextPage endCursor }
  }
  merged: search(query: $mergedQuery, type: ISSUE, first: 100, after: $mergedAfter)
    @include(if: $includeMerged) {
    nodes { ... on PullRequest { ...TrouvePullRequestProbe } }
    pageInfo { hasNextPage endCursor }
  }
  rateLimit { cost remaining resetAt }
}

fragment TrouvePullRequestProbe on PullRequest {
  id
  updatedAt
  headRefOid
  mergeable
  commits(last: 1) {
    nodes {
      commit {
        statusCheckRollup { state }
      }
    }
  }
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

/// Full selected-PR collaboration state. The large connections are aliased so
/// they can coexist with the compact summary fragment and paged independently.
const PULL_REQUEST_DETAIL_QUERY: &str = r#"
query TrouvePullRequestDetail(
  $owner: String!, $repository: String!, $number: Int!,
  $commentsAfter: String, $threadsAfter: String, $reviewsAfter: String,
  $commitsAfter: String, $filesAfter: String,
  $loadComments: Boolean!, $loadThreads: Boolean!, $loadReviews: Boolean!,
  $loadCommits: Boolean!, $loadFiles: Boolean!
) {
  viewer { login }
  repository(owner: $owner, name: $repository) {
    mergeCommitAllowed
    squashMergeAllowed
    rebaseMergeAllowed
    autoMergeAllowed
    viewerDefaultMergeMethod
    labels(first: 100, orderBy: { field: NAME, direction: ASC }) {
      nodes { id name color description }
    }
    milestones(first: 100, states: [OPEN], orderBy: { field: DUE_DATE, direction: ASC }) {
      nodes { id number title state url }
    }
    assignableUsers(first: 100) {
      nodes { id login name avatarUrl url }
    }
    pullRequest(number: $number) {
      ...TrouvePullRequestFields
      baseRefOid
      body
      viewerSubscription
      reactionGroups { content viewerHasReacted users { totalCount } }
      createdAt
      updatedAt
      additions
      deletions
      changedFiles
      reviewDecision
      locked
      activeLockReason
      maintainerCanModify
      viewerCanUpdate
      viewerCanClose
      viewerCanReopen
      viewerCanAssign
      viewerCanLabel
      viewerCanMergeAsAdmin
      viewerCanUpdateBranch
      viewerCanEnableAutoMerge
      viewerCanDisableAutoMerge
      viewerDidAuthor
      isMergeQueueEnabled
      mergeQueueEntry {
        id position state enqueuedAt estimatedTimeToMerge
      }
      autoMergeRequest {
        enabledAt mergeMethod commitHeadline commitBody
        enabledBy { login avatarUrl url ... on User { id name } ... on Bot { id } }
      }
      labels(first: 100) { nodes { id name color description } }
      assignees(first: 100) { nodes { id login name avatarUrl url } }
      milestone { id number title state url }
      detailReviewRequests: reviewRequests(first: 100) {
        nodes {
          requestedReviewer {
            __typename
            ... on User { id login name avatarUrl url }
            ... on Bot { id login avatarUrl url }
            ... on Team { id name slug avatarUrl url }
            ... on Mannequin { id login avatarUrl url }
          }
        }
      }
      ...TrouvePullRequestDetailConnections
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

/// Continuation pages intentionally omit the expensive summary fragment,
/// repository metadata, viewer, labels, milestones, assignees, and stack.
/// Those values are immutable for one detail response and come from page one.
const PULL_REQUEST_DETAIL_PAGE_QUERY: &str = r#"
query TrouvePullRequestDetailPage(
  $owner: String!, $repository: String!, $number: Int!,
  $commentsAfter: String, $threadsAfter: String, $reviewsAfter: String,
  $commitsAfter: String, $filesAfter: String,
  $loadComments: Boolean!, $loadThreads: Boolean!, $loadReviews: Boolean!,
  $loadCommits: Boolean!, $loadFiles: Boolean!
) {
  repository(owner: $owner, name: $repository) {
    pullRequest(number: $number) { ...TrouvePullRequestDetailConnections }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const PULL_REQUEST_DETAIL_CONNECTIONS: &str = r#"
fragment TrouvePullRequestDetailConnections on PullRequest {
  detailComments: comments(first: 100, after: $commentsAfter) @include(if: $loadComments) {
    nodes {
      id databaseId body url createdAt updatedAt lastEditedAt
      viewerCanUpdate viewerCanDelete viewerDidAuthor
      author { login avatarUrl url ... on User { id name } ... on Bot { id } }
      reactionGroups { content viewerHasReacted users { totalCount } }
    }
    totalCount
    pageInfo { hasNextPage endCursor }
  }
  detailReviewThreads: reviewThreads(first: 100, after: $threadsAfter) @include(if: $loadThreads) {
    nodes {
      id path line startLine diffSide isOutdated isResolved
      viewerCanReply viewerCanResolve viewerCanUnresolve
      comments(first: 100) {
        nodes {
          id body url createdAt updatedAt lastEditedAt
          viewerCanUpdate viewerCanDelete viewerDidAuthor
          path line diffHunk
          author { login avatarUrl url ... on User { id name } ... on Bot { id } }
          reactionGroups { content viewerHasReacted users { totalCount } }
        }
      }
    }
    totalCount
    pageInfo { hasNextPage endCursor }
  }
  detailReviews: reviews(first: 100, after: $reviewsAfter) @include(if: $loadReviews) {
    nodes {
      id body state url submittedAt viewerCanUpdate viewerCanDelete viewerDidAuthor
      author { login avatarUrl url ... on User { id name } ... on Bot { id } }
      commit { oid }
    }
    totalCount
    pageInfo { hasNextPage endCursor }
  }
  detailCommits: commits(first: 100, after: $commitsAfter) @include(if: $loadCommits) {
    nodes {
      commit {
        oid abbreviatedOid messageHeadline messageBody committedDate url
        author { user { id login name avatarUrl url } name }
      }
    }
    totalCount
    pageInfo { hasNextPage endCursor }
  }
  detailFiles: files(first: 100, after: $filesAfter) @include(if: $loadFiles) {
    nodes { path additions deletions changeType viewerViewedState }
    totalCount
    pageInfo { hasNextPage endCursor }
  }
}
"#;

/// GitHub's native stack fields are new and not present on every GHES
/// version. Fetch them independently so an unsupported stack schema never
/// hides the rest of the PR page.
const PULL_REQUEST_STACK_QUERY: &str = r#"
query TrouvePullRequestStack($owner: String!, $repository: String!, $number: Int!) {
  repository(owner: $owner, name: $repository) {
    pullRequest(number: $number) {
      stack {
        id number size baseRefName
        entries(first: 100) {
          nodes {
            position
            pullRequest {
              number title url state isDraft baseRefName headRefName
              reviewDecision mergeStateStatus
            }
          }
        }
      }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

/// Resolve the selected changed file and the immutable base/head object ids
/// without downloading the pull request's aggregate patch.
const PULL_REQUEST_FILE_QUERY: &str = r#"
query TrouvePullRequestFile(
  $owner: String!, $repository: String!, $number: Int!, $after: String
) {
  repository(owner: $owner, name: $repository) {
    pullRequest(number: $number) {
      baseRefOid
      headRefOid
      files(first: 100, after: $after) {
        nodes { path additions deletions changeType viewerViewedState }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const PULL_REQUEST_BLOB_METADATA_QUERY: &str = r#"
query TrouvePullRequestBlobMetadata(
  $owner: String!, $repository: String!, $base: String!, $head: String!
) {
  repository(owner: $owner, name: $repository) {
    base: object(expression: $base) {
      __typename
      ... on Blob { byteSize isBinary isTruncated }
    }
    head: object(expression: $head) {
      __typename
      ... on Blob { byteSize isBinary isTruncated }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const PULL_REQUEST_BLOB_TEXT_QUERY: &str = r#"
query TrouvePullRequestBlobText(
  $owner: String!, $repository: String!, $base: String!, $head: String!,
  $loadBase: Boolean!, $loadHead: Boolean!
) {
  repository(owner: $owner, name: $repository) {
    base: object(expression: $base) {
      __typename
      ... on Blob { text @include(if: $loadBase) }
    }
    head: object(expression: $head) {
      __typename
      ... on Blob { text @include(if: $loadHead) }
    }
  }
  rateLimit { cost remaining resetAt }
}
"#;

const UPDATE_PULL_REQUEST_MUTATION: &str = r#"
mutation TrouveUpdatePullRequest($input: UpdatePullRequestInput!) {
  updatePullRequest(input: $input) { pullRequest { id } }
}
"#;

const CLOSE_PULL_REQUEST_MUTATION: &str = r#"
mutation TrouveClosePullRequest($input: ClosePullRequestInput!) {
  closePullRequest(input: $input) { pullRequest { id } }
}
"#;

const REOPEN_PULL_REQUEST_MUTATION: &str = r#"
mutation TrouveReopenPullRequest($input: ReopenPullRequestInput!) {
  reopenPullRequest(input: $input) { pullRequest { id } }
}
"#;

const CONVERT_PULL_REQUEST_TO_DRAFT_MUTATION: &str = r#"
mutation TrouveConvertPullRequestToDraft($input: ConvertPullRequestToDraftInput!) {
  convertPullRequestToDraft(input: $input) { pullRequest { id } }
}
"#;

const MARK_PULL_REQUEST_READY_MUTATION: &str = r#"
mutation TrouveMarkPullRequestReady($input: MarkPullRequestReadyForReviewInput!) {
  markPullRequestReadyForReview(input: $input) { pullRequest { id } }
}
"#;

const REQUEST_REVIEWS_MUTATION: &str = r#"
mutation TrouveRequestReviews($input: RequestReviewsByLoginInput!) {
  requestReviewsByLogin(input: $input) { pullRequest { id } }
}
"#;

const ADD_REVIEW_MUTATION: &str = r#"
mutation TrouveAddReview($input: AddPullRequestReviewInput!) {
  addPullRequestReview(input: $input) { pullRequestReview { id } }
}
"#;

const SUBMIT_REVIEW_MUTATION: &str = r#"
mutation TrouveSubmitReview($input: SubmitPullRequestReviewInput!) {
  submitPullRequestReview(input: $input) { pullRequestReview { id } }
}
"#;

const UPDATE_REVIEW_MUTATION: &str = r#"
mutation TrouveUpdateReview($input: UpdatePullRequestReviewInput!) {
  updatePullRequestReview(input: $input) { pullRequestReview { id } }
}
"#;

const DELETE_REVIEW_MUTATION: &str = r#"
mutation TrouveDeleteReview($input: DeletePullRequestReviewInput!) {
  deletePullRequestReview(input: $input) { clientMutationId }
}
"#;

const DISMISS_REVIEW_MUTATION: &str = r#"
mutation TrouveDismissReview($input: DismissPullRequestReviewInput!) {
  dismissPullRequestReview(input: $input) { pullRequestReview { id } }
}
"#;

const ADD_COMMENT_MUTATION: &str = r#"
mutation TrouveAddComment($input: AddCommentInput!) {
  addComment(input: $input) { commentEdge { node { id } } }
}
"#;

const UPDATE_ISSUE_COMMENT_MUTATION: &str = r#"
mutation TrouveUpdateIssueComment($input: UpdateIssueCommentInput!) {
  updateIssueComment(input: $input) { issueComment { id } }
}
"#;

const DELETE_ISSUE_COMMENT_MUTATION: &str = r#"
mutation TrouveDeleteIssueComment($input: DeleteIssueCommentInput!) {
  deleteIssueComment(input: $input) { clientMutationId }
}
"#;

const UPDATE_REVIEW_COMMENT_MUTATION: &str = r#"
mutation TrouveUpdateReviewComment($input: UpdatePullRequestReviewCommentInput!) {
  updatePullRequestReviewComment(input: $input) { pullRequestReviewComment { id } }
}
"#;

const DELETE_REVIEW_COMMENT_MUTATION: &str = r#"
mutation TrouveDeleteReviewComment($input: DeletePullRequestReviewCommentInput!) {
  deletePullRequestReviewComment(input: $input) { clientMutationId }
}
"#;

const REPLY_REVIEW_THREAD_MUTATION: &str = r#"
mutation TrouveReplyReviewThread($input: AddPullRequestReviewThreadReplyInput!) {
  addPullRequestReviewThreadReply(input: $input) { comment { id } }
}
"#;

const RESOLVE_REVIEW_THREAD_MUTATION: &str = r#"
mutation TrouveResolveReviewThread($input: ResolveReviewThreadInput!) {
  resolveReviewThread(input: $input) { thread { id } }
}
"#;

const UNRESOLVE_REVIEW_THREAD_MUTATION: &str = r#"
mutation TrouveUnresolveReviewThread($input: UnresolveReviewThreadInput!) {
  unresolveReviewThread(input: $input) { thread { id } }
}
"#;

const ADD_REVIEW_THREAD_MUTATION: &str = r#"
mutation TrouveAddReviewThread($input: AddPullRequestReviewThreadInput!) {
  addPullRequestReviewThread(input: $input) { thread { id } }
}
"#;

const MARK_FILE_VIEWED_MUTATION: &str = r#"
mutation TrouveMarkFileViewed($input: MarkFileAsViewedInput!) {
  markFileAsViewed(input: $input) { pullRequest { id } }
}
"#;

const UNMARK_FILE_VIEWED_MUTATION: &str = r#"
mutation TrouveUnmarkFileViewed($input: UnmarkFileAsViewedInput!) {
  unmarkFileAsViewed(input: $input) { pullRequest { id } }
}
"#;

const UPDATE_PULL_REQUEST_BRANCH_MUTATION: &str = r#"
mutation TrouveUpdatePullRequestBranch($input: UpdatePullRequestBranchInput!) {
  updatePullRequestBranch(input: $input) { pullRequest { id } }
}
"#;

const MERGE_PULL_REQUEST_MUTATION: &str = r#"
mutation TrouveMergePullRequest($input: MergePullRequestInput!) {
  mergePullRequest(input: $input) { pullRequest { id merged } }
}
"#;

const ENABLE_AUTO_MERGE_MUTATION: &str = r#"
mutation TrouveEnableAutoMerge($input: EnablePullRequestAutoMergeInput!) {
  enablePullRequestAutoMerge(input: $input) { pullRequest { id } }
}
"#;

const DISABLE_AUTO_MERGE_MUTATION: &str = r#"
mutation TrouveDisableAutoMerge($input: DisablePullRequestAutoMergeInput!) {
  disablePullRequestAutoMerge(input: $input) { pullRequest { id } }
}
"#;

const ENQUEUE_PULL_REQUEST_MUTATION: &str = r#"
mutation TrouveEnqueuePullRequest($input: EnqueuePullRequestInput!) {
  enqueuePullRequest(input: $input) { mergeQueueEntry { id } }
}
"#;

const DEQUEUE_PULL_REQUEST_MUTATION: &str = r#"
mutation TrouveDequeuePullRequest($input: DequeuePullRequestInput!) {
  dequeuePullRequest(input: $input) { mergeQueueEntry { id } }
}
"#;

const LOCK_LOCKABLE_MUTATION: &str = r#"
mutation TrouveLockConversation($input: LockLockableInput!) {
  lockLockable(input: $input) { lockedRecord { locked } }
}
"#;

const UNLOCK_LOCKABLE_MUTATION: &str = r#"
mutation TrouveUnlockConversation($input: UnlockLockableInput!) {
  unlockLockable(input: $input) { unlockedRecord { locked } }
}
"#;

const ADD_REACTION_MUTATION: &str = r#"
mutation TrouveAddReaction($input: AddReactionInput!) {
  addReaction(input: $input) { reaction { content } }
}
"#;

const REMOVE_REACTION_MUTATION: &str = r#"
mutation TrouveRemoveReaction($input: RemoveReactionInput!) {
  removeReaction(input: $input) { reaction { content } }
}
"#;

const UPDATE_SUBSCRIPTION_MUTATION: &str = r#"
mutation TrouveUpdateSubscription($input: UpdateSubscriptionInput!) {
  updateSubscription(input: $input) { subscribable { viewerSubscription } }
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
  id
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
  mergeStateStatus
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
                detailsUrl
                startedAt
                completedAt
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

/// Guard against pathological PRs while still covering GitHub's own largest
/// practical conversations. Connections are fetched in pages of 100.
const MAX_PR_DETAIL_PAGES: usize = 20;

/// File lookup stays bounded independently from the richer PR detail
/// connections. GitHub itself caps changed files at 3,000, so 30 pages covers
/// the complete public surface without an open-ended request loop.
const MAX_PR_FILE_PAGES: usize = 30;

/// CodeMirror switches to a virtualized before/after presentation at this
/// aggregate size. Keep each remote blob bounded at the same threshold so the
/// protocol never transports an unexpectedly huge GitHub object.
const MAX_PR_FILE_TEXT_BYTES: u64 = 3_000_000;

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

/// Per-host dashboard state retained by the server between poll ticks.
///
/// Search probes are deliberately cheap. Full PR details are fetched only
/// when a probe changes, which keeps the 30-second dashboard cadence without
/// paying the nested-connection cost on every unchanged pull request.
#[derive(Default)]
pub struct GitHubDashboardCache {
    viewer: Option<String>,
    entries: HashMap<String, CachedDashboardPullRequest>,
    published_snapshot: Option<String>,
    last_successful_refresh: Option<Instant>,
}

struct CachedDashboardPullRequest {
    fingerprint: DashboardFingerprint,
    pull_request: PrInfo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DashboardFingerprint {
    updated_at: DateTime<Utc>,
    head_ref_oid: Option<String>,
    mergeable: String,
    check_state: Option<String>,
}

impl GitHubDashboardCache {
    fn begin_viewer(&mut self, viewer: &str) {
        if self.viewer.as_deref() != Some(viewer) {
            self.entries.clear();
            self.viewer = Some(viewer.to_string());
        }
    }

    fn needs_detail_refresh(&self, id: &str, fingerprint: &DashboardFingerprint) -> bool {
        self.entries
            .get(id)
            .is_none_or(|cached| cached.fingerprint != *fingerprint)
    }

    /// Serialize once for an exact, stable comparison with the last snapshot
    /// emitted by this server process. Returning the serialized value lets the
    /// caller remember it only after the durable event append succeeds.
    pub(crate) fn unpublished_snapshot(&self, snapshot: &GithubPrList) -> Result<Option<String>> {
        let serialized = serde_json::to_string(snapshot)?;
        Ok((self.published_snapshot.as_deref() != Some(serialized.as_str())).then_some(serialized))
    }

    pub(crate) fn has_published_snapshot(&self) -> bool {
        self.published_snapshot.is_some()
    }

    pub(crate) fn seed_published_snapshot(&mut self, snapshot: &GithubPrList) -> Result<()> {
        if self.published_snapshot.is_none() {
            self.published_snapshot = Some(serde_json::to_string(snapshot)?);
        }
        Ok(())
    }

    pub(crate) fn mark_snapshot_published(&mut self, serialized: String) {
        self.published_snapshot = Some(serialized);
    }

    pub(crate) fn should_refresh(
        &self,
        force: bool,
        request_started: Instant,
        now: Instant,
        freshness: Duration,
    ) -> bool {
        let Some(last_refresh) = self.last_successful_refresh else {
            return true;
        };
        if force {
            // A manual request waits for an in-flight refresh. Reuse that
            // result when it completed after the click instead of immediately
            // issuing an identical forced refresh.
            last_refresh < request_started
        } else {
            now.saturating_duration_since(last_refresh) >= freshness
        }
    }

    pub(crate) fn mark_refresh_completed(&mut self) {
        self.last_successful_refresh = Some(Instant::now());
    }
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

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlActor {
    #[serde(default)]
    id: String,
    login: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    avatar_url: String,
    #[serde(default)]
    url: String,
    #[serde(default, rename = "__typename")]
    typename: String,
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
    nodes: Vec<Option<GraphqlDashboardProbe>>,
    page_info: GraphqlPageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlDashboardProbe {
    id: String,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    head_ref_oid: Option<String>,
    mergeable: String,
    commits: GraphqlProbeCommits,
}

impl GraphqlDashboardProbe {
    fn into_entry(self) -> (String, DashboardFingerprint) {
        let check_state = self
            .commits
            .nodes
            .into_iter()
            .flatten()
            .find_map(|commit| commit.commit.status_check_rollup.map(|rollup| rollup.state));
        (
            self.id,
            DashboardFingerprint {
                updated_at: self.updated_at,
                head_ref_oid: self.head_ref_oid,
                mergeable: self.mergeable,
                check_state,
            },
        )
    }
}

#[derive(Deserialize)]
struct GraphqlProbeCommits {
    #[serde(default)]
    nodes: Vec<Option<GraphqlProbePullRequestCommit>>,
}

#[derive(Deserialize)]
struct GraphqlProbePullRequestCommit {
    commit: GraphqlProbeCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlProbeCommit {
    status_check_rollup: Option<GraphqlProbeStatusCheckRollup>,
}

#[derive(Deserialize)]
struct GraphqlProbeStatusCheckRollup {
    state: String,
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
struct GraphqlPrFileData {
    repository: Option<GraphqlPrFileRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrFileRepository {
    pull_request: Option<GraphqlPrFilePullRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrFilePullRequest {
    base_ref_oid: String,
    head_ref_oid: String,
    files: GraphqlPagedNodes<GraphqlDetailFile>,
}

#[derive(Deserialize)]
struct GraphqlBlobData {
    repository: Option<GraphqlBlobRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
struct GraphqlBlobRepository {
    base: Option<GraphqlBlob>,
    head: Option<GraphqlBlob>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlBlob {
    #[serde(default, rename = "__typename")]
    typename: String,
    #[serde(default)]
    byte_size: Option<u64>,
    #[serde(default)]
    is_binary: Option<bool>,
    #[serde(default)]
    is_truncated: Option<bool>,
    #[serde(default)]
    text: Option<String>,
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
    id: String,
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
    #[serde(default)]
    merge_state_status: Option<String>,
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
#[serde(rename_all = "camelCase")]
struct GraphqlRequestedReviewer {
    #[serde(default)]
    id: String,
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    avatar_url: String,
    #[serde(default)]
    url: String,
    #[serde(default, rename = "__typename")]
    typename: String,
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
    #[serde(default, rename = "detailsUrl")]
    details_url: Option<String>,
    #[serde(default, rename = "startedAt")]
    started_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "completedAt")]
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct GraphqlPrDetailData {
    viewer: GraphqlActor,
    repository: Option<GraphqlPrDetailRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
struct GraphqlPrDetailPageData {
    repository: Option<GraphqlPrDetailPageRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrDetailPageRepository {
    pull_request: Option<GraphqlPrDetailPageNode>,
}

#[derive(Deserialize)]
struct GraphqlPrDetailPageNode {
    #[serde(flatten)]
    connections: GraphqlPrDetailConnections,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrDetailRepository {
    merge_commit_allowed: bool,
    squash_merge_allowed: bool,
    rebase_merge_allowed: bool,
    auto_merge_allowed: bool,
    viewer_default_merge_method: String,
    labels: GraphqlNodes<GraphqlDetailLabel>,
    milestones: GraphqlNodes<GraphqlDetailMilestone>,
    assignable_users: GraphqlNodes<GraphqlActor>,
    pull_request: Option<GraphqlPrDetailNode>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct GraphqlNodes<T> {
    #[serde(default)]
    nodes: Vec<Option<T>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct GraphqlPagedNodes<T> {
    #[serde(default)]
    nodes: Vec<Option<T>>,
    page_info: GraphqlPageInfo,
    #[serde(default)]
    total_count: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrDetailNode {
    #[serde(flatten)]
    summary: GraphqlPullRequest,
    base_ref_oid: String,
    body: String,
    viewer_subscription: String,
    #[serde(default)]
    reaction_groups: Vec<GraphqlReactionGroup>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    #[serde(default)]
    review_decision: Option<String>,
    locked: bool,
    #[serde(default)]
    active_lock_reason: Option<String>,
    maintainer_can_modify: bool,
    viewer_can_update: bool,
    viewer_can_close: bool,
    viewer_can_reopen: bool,
    viewer_can_assign: bool,
    viewer_can_label: bool,
    viewer_can_merge_as_admin: bool,
    viewer_can_update_branch: bool,
    viewer_can_enable_auto_merge: bool,
    viewer_can_disable_auto_merge: bool,
    viewer_did_author: bool,
    is_merge_queue_enabled: bool,
    #[serde(default)]
    merge_queue_entry: Option<GraphqlMergeQueueEntry>,
    #[serde(default)]
    auto_merge_request: Option<GraphqlAutoMerge>,
    labels: GraphqlNodes<GraphqlDetailLabel>,
    assignees: GraphqlNodes<GraphqlActor>,
    #[serde(default)]
    milestone: Option<GraphqlDetailMilestone>,
    #[serde(rename = "detailReviewRequests")]
    detail_review_requests: GraphqlNodes<GraphqlDetailReviewRequest>,
    #[serde(flatten)]
    connections: GraphqlPrDetailConnections,
}

#[derive(Default, Deserialize)]
struct GraphqlPrDetailConnections {
    #[serde(default, rename = "detailComments")]
    detail_comments: Option<GraphqlPagedNodes<GraphqlDetailComment>>,
    #[serde(default, rename = "detailReviewThreads")]
    detail_review_threads: Option<GraphqlPagedNodes<GraphqlDetailReviewThread>>,
    #[serde(default, rename = "detailReviews")]
    detail_reviews: Option<GraphqlPagedNodes<GraphqlDetailReview>>,
    #[serde(default, rename = "detailCommits")]
    detail_commits: Option<GraphqlPagedNodes<GraphqlDetailPullRequestCommit>>,
    #[serde(default, rename = "detailFiles")]
    detail_files: Option<GraphqlPagedNodes<GraphqlDetailFile>>,
}

#[derive(Deserialize)]
struct GraphqlDetailLabel {
    id: String,
    name: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct GraphqlDetailMilestone {
    id: String,
    number: u64,
    title: String,
    state: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlMergeQueueEntry {
    id: String,
    position: u64,
    state: String,
    enqueued_at: DateTime<Utc>,
    #[serde(default)]
    estimated_time_to_merge: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlAutoMerge {
    enabled_at: DateTime<Utc>,
    merge_method: String,
    #[serde(default)]
    commit_headline: Option<String>,
    #[serde(default)]
    commit_body: Option<String>,
    #[serde(default)]
    enabled_by: Option<GraphqlActor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlDetailReviewRequest {
    requested_reviewer: Option<GraphqlRequestedReviewer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlDetailComment {
    id: String,
    #[serde(default)]
    database_id: Option<u64>,
    body: String,
    url: String,
    author: Option<GraphqlActor>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    last_edited_at: Option<DateTime<Utc>>,
    viewer_can_update: bool,
    viewer_can_delete: bool,
    viewer_did_author: bool,
    #[serde(default)]
    reaction_groups: Vec<GraphqlReactionGroup>,
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    diff_hunk: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlReactionGroup {
    content: String,
    viewer_has_reacted: bool,
    users: GraphqlCount,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlCount {
    total_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlDetailReviewThread {
    id: String,
    path: String,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    start_line: Option<u64>,
    diff_side: String,
    is_outdated: bool,
    is_resolved: bool,
    viewer_can_reply: bool,
    viewer_can_resolve: bool,
    viewer_can_unresolve: bool,
    comments: GraphqlNodes<GraphqlDetailComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlDetailReview {
    id: String,
    author: Option<GraphqlActor>,
    state: String,
    #[serde(default)]
    body: String,
    url: String,
    #[serde(default)]
    submitted_at: Option<DateTime<Utc>>,
    viewer_can_update: bool,
    viewer_can_delete: bool,
    viewer_did_author: bool,
    #[serde(default)]
    commit: Option<GraphqlOid>,
}

#[derive(Deserialize)]
struct GraphqlOid {
    oid: String,
}

#[derive(Deserialize)]
struct GraphqlDetailPullRequestCommit {
    commit: GraphqlDetailCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlDetailCommit {
    oid: String,
    abbreviated_oid: String,
    message_headline: String,
    #[serde(default)]
    message_body: String,
    committed_date: DateTime<Utc>,
    url: String,
    #[serde(default)]
    author: Option<GraphqlCommitAuthor>,
}

#[derive(Deserialize)]
struct GraphqlCommitAuthor {
    #[serde(default)]
    user: Option<GraphqlActor>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlDetailFile {
    path: String,
    additions: u64,
    deletions: u64,
    change_type: String,
    viewer_viewed_state: String,
}

#[derive(Deserialize)]
struct GraphqlPrStackData {
    repository: Option<GraphqlPrStackRepository>,
    #[serde(rename = "rateLimit")]
    rate_limit: GraphqlRateLimit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrStackRepository {
    pull_request: Option<GraphqlPrStackPullRequest>,
}

#[derive(Deserialize)]
struct GraphqlPrStackPullRequest {
    stack: Option<GraphqlPrStack>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrStack {
    id: String,
    number: u64,
    size: u64,
    base_ref_name: String,
    entries: GraphqlNodes<GraphqlPrStackEntry>,
}

#[derive(Deserialize)]
struct GraphqlPrStackEntry {
    position: u64,
    #[serde(rename = "pullRequest")]
    pull_request: Option<GraphqlPrStackEntryPullRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlPrStackEntryPullRequest {
    number: u64,
    title: String,
    url: String,
    state: String,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    merge_state_status: Option<String>,
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

struct PrDetailPagination {
    comments_after: Option<String>,
    threads_after: Option<String>,
    reviews_after: Option<String>,
    commits_after: Option<String>,
    files_after: Option<String>,
    load_comments: bool,
    load_threads: bool,
    load_reviews: bool,
    load_commits: bool,
    load_files: bool,
    comments: Vec<GraphqlDetailComment>,
    threads: Vec<GraphqlDetailReviewThread>,
    reviews: Vec<GraphqlDetailReview>,
    commits: Vec<GraphqlDetailPullRequestCommit>,
    files: Vec<GraphqlDetailFile>,
    commit_count: u64,
}

impl PrDetailPagination {
    fn new(sections: &HashSet<PrDetailSection>) -> Self {
        let load_conversation = sections.contains(&PrDetailSection::Conversation);
        Self {
            comments_after: None,
            threads_after: None,
            reviews_after: None,
            commits_after: None,
            files_after: None,
            load_comments: load_conversation,
            load_threads: load_conversation,
            load_reviews: load_conversation,
            load_commits: sections.contains(&PrDetailSection::Commits),
            load_files: sections.contains(&PrDetailSection::Files),
            comments: Vec::new(),
            threads: Vec::new(),
            reviews: Vec::new(),
            commits: Vec::new(),
            files: Vec::new(),
            commit_count: 0,
        }
    }

    fn active(&self) -> bool {
        self.load_comments
            || self.load_threads
            || self.load_reviews
            || self.load_commits
            || self.load_files
    }

    fn consume(&mut self, mut connections: GraphqlPrDetailConnections) {
        consume_detail_page(
            connections.detail_comments.take(),
            &mut self.comments,
            &mut self.comments_after,
            &mut self.load_comments,
        );
        consume_detail_page(
            connections.detail_review_threads.take(),
            &mut self.threads,
            &mut self.threads_after,
            &mut self.load_threads,
        );
        consume_detail_page(
            connections.detail_reviews.take(),
            &mut self.reviews,
            &mut self.reviews_after,
            &mut self.load_reviews,
        );
        if let Some(page) = connections.detail_commits.take() {
            self.commit_count = self.commit_count.max(page.total_count.unwrap_or_default());
            consume_detail_page(
                Some(page),
                &mut self.commits,
                &mut self.commits_after,
                &mut self.load_commits,
            );
        } else {
            self.load_commits = false;
        }
        consume_detail_page(
            connections.detail_files.take(),
            &mut self.files,
            &mut self.files_after,
            &mut self.load_files,
        );
    }

    fn apply_to(self, mut detail: PrDetail, sections: &HashSet<PrDetailSection>) -> PrDetail {
        let truncated = self.active();
        if sections.contains(&PrDetailSection::Conversation) {
            detail.comments = self
                .comments
                .into_iter()
                .map(GraphqlDetailComment::into_pr_comment)
                .collect();
            detail.review_threads = self
                .threads
                .into_iter()
                .map(GraphqlDetailReviewThread::into_pr_thread)
                .collect();
            detail.reviews = self
                .reviews
                .into_iter()
                .map(GraphqlDetailReview::into_pr_review)
                .collect();
        }
        if sections.contains(&PrDetailSection::Commits) {
            detail.commit_count = self.commit_count;
            detail.commits = self
                .commits
                .into_iter()
                .map(GraphqlDetailPullRequestCommit::into_pr_commit)
                .collect();
        }
        if sections.contains(&PrDetailSection::Files) {
            detail.files = self
                .files
                .into_iter()
                .map(GraphqlDetailFile::into_pr_file)
                .collect();
        }
        detail.truncated |= truncated;
        detail
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

    async fn dashboard_prs(
        &self,
        merged_since: DateTime<Utc>,
        cache: &mut GitHubDashboardCache,
    ) -> Result<(String, Vec<PrInfo>)> {
        let viewer = self.viewer().await?;
        cache.begin_viewer(&viewer);
        let day = merged_since.format("%Y-%m-%d");
        let mut open = SearchCursor::new(format!("is:pr is:open involves:{viewer}"));
        let mut review = SearchCursor::new(format!("is:pr is:open review-requested:{viewer}"));
        let mut merged =
            SearchCursor::new(format!("is:pr is:merged merged:>={day} involves:{viewer}"));
        let mut probes = BTreeMap::new();

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
            consume_search_page(response.open, &mut open, &mut probes);
            consume_search_page(response.review, &mut review, &mut probes);
            consume_search_page(response.merged, &mut merged, &mut probes);
        }

        let refresh_ids = probes
            .iter()
            .filter(|(id, fingerprint)| cache.needs_detail_refresh(id, fingerprint))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        let refreshed = self.pull_requests_by_id(&refresh_ids).await?;

        // A successful details query is authoritative even when GitHub
        // returns null for an id (for example, access disappeared between
        // search and enrichment). Do not keep the old snapshot in that case.
        for id in &refresh_ids {
            cache.entries.remove(id);
        }
        for (id, pull_request) in refreshed {
            if let Some(fingerprint) = probes.get(&id) {
                cache.entries.insert(
                    id,
                    CachedDashboardPullRequest {
                        fingerprint: fingerprint.clone(),
                        pull_request,
                    },
                );
            }
        }
        cache.entries.retain(|id, _| probes.contains_key(id));

        let mut prs = probes
            .keys()
            .filter_map(|id| cache.entries.get(id))
            .map(|cached| cached.pull_request.clone())
            .collect::<Vec<_>>();
        prs.sort_by_key(|pr| std::cmp::Reverse(pr.number));
        Ok((viewer, prs))
    }

    async fn pull_requests_by_id(&self, ids: &[String]) -> Result<Vec<(String, PrInfo)>> {
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
            prs.extend(response.nodes.into_iter().flatten().map(|pr| {
                let id = pr.id.clone();
                (id, pr.into_pr_info(&self.host))
            }));
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

    async fn pull_request_detail(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
        sections: &HashSet<PrDetailSection>,
        existing: Option<PrDetail>,
    ) -> Result<PrDetail> {
        if !sections.contains(&PrDetailSection::Overview)
            && let Some(detail) = existing
        {
            let pagination = self
                .load_pull_request_detail_pages(
                    owner,
                    repository,
                    number,
                    PrDetailPagination::new(sections),
                    0,
                )
                .await?;
            return Ok(pagination.apply_to(detail, sections));
        }
        let query = operation_with_pr_detail_fields(PULL_REQUEST_DETAIL_QUERY);
        let mut pagination = PrDetailPagination::new(sections);
        let response: GraphqlPrDetailData = self
            .client
            .graphql(&serde_json::json!({
                "query": query,
                "variables": {
                    "owner": owner,
                    "repository": repository,
                    "number": number,
                    "commentsAfter": pagination.comments_after.as_deref(),
                    "threadsAfter": pagination.threads_after.as_deref(),
                    "reviewsAfter": pagination.reviews_after.as_deref(),
                    "commitsAfter": pagination.commits_after.as_deref(),
                    "filesAfter": pagination.files_after.as_deref(),
                    "loadComments": pagination.load_comments,
                    "loadThreads": pagination.load_threads,
                    "loadReviews": pagination.load_reviews,
                    "loadCommits": pagination.load_commits,
                    "loadFiles": pagination.load_files,
                }
            }))
            .await
            .context("loading full pull request detail through GraphQL")?;
        self.trace_rate("full pull request detail", &response.rate_limit);
        let viewer = response.viewer.login;
        let mut repository_detail = response
            .repository
            .with_context(|| format!("repository {owner}/{repository} not found"))?;
        let mut pull_request = repository_detail
            .pull_request
            .take()
            .with_context(|| format!("pull request #{number} not found"))?;
        pagination.consume(std::mem::take(&mut pull_request.connections));
        pagination = self
            .load_pull_request_detail_pages(owner, repository, number, pagination, 1)
            .await?;

        let truncated = pagination.active();
        let stack = if sections.contains(&PrDetailSection::Overview) {
            match self.pull_request_stack(owner, repository, number).await {
                Ok(stack) => stack,
                Err(error) => {
                    tracing::debug!(
                        host = self.host,
                        owner,
                        repository,
                        number,
                        error = %error,
                        "GitHub host does not expose native pull-request stacks"
                    );
                    None
                }
            }
        } else {
            None
        };
        pull_request.into_pr_detail(
            &self.host,
            viewer,
            repository_detail,
            pagination.comments,
            pagination.threads,
            pagination.reviews,
            pagination.commits,
            pagination.files,
            pagination.commit_count,
            stack,
            truncated,
        )
    }

    async fn load_pull_request_detail_pages(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
        mut pagination: PrDetailPagination,
        mut page_count: usize,
    ) -> Result<PrDetailPagination> {
        let page_query = operation_with_pr_detail_connections(PULL_REQUEST_DETAIL_PAGE_QUERY);
        while pagination.active() && page_count < MAX_PR_DETAIL_PAGES {
            let response: GraphqlPrDetailPageData = self
                .client
                .graphql(&serde_json::json!({
                    "query": page_query,
                    "variables": {
                        "owner": owner,
                        "repository": repository,
                        "number": number,
                        "commentsAfter": pagination.comments_after.as_deref(),
                        "threadsAfter": pagination.threads_after.as_deref(),
                        "reviewsAfter": pagination.reviews_after.as_deref(),
                        "commitsAfter": pagination.commits_after.as_deref(),
                        "filesAfter": pagination.files_after.as_deref(),
                        "loadComments": pagination.load_comments,
                        "loadThreads": pagination.load_threads,
                        "loadReviews": pagination.load_reviews,
                        "loadCommits": pagination.load_commits,
                        "loadFiles": pagination.load_files,
                    }
                }))
                .await
                .context("loading pull request detail continuation through GraphQL")?;
            self.trace_rate("pull request detail continuation", &response.rate_limit);
            let repository_page = response
                .repository
                .with_context(|| format!("repository {owner}/{repository} not found"))?;
            let page = repository_page
                .pull_request
                .with_context(|| format!("pull request #{number} not found"))?;
            pagination.consume(page.connections);
            page_count += 1;
        }
        Ok(pagination)
    }

    async fn pull_request_stack(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
    ) -> Result<Option<PrStack>> {
        let response: GraphqlPrStackData = self
            .client
            .graphql(&serde_json::json!({
                "query": PULL_REQUEST_STACK_QUERY,
                "variables": {
                    "owner": owner,
                    "repository": repository,
                    "number": number,
                }
            }))
            .await
            .context("loading native pull-request stack through GraphQL")?;
        self.trace_rate("pull request stack", &response.rate_limit);
        Ok(response
            .repository
            .and_then(|repository| repository.pull_request)
            .and_then(|pull_request| pull_request.stack)
            .map(GraphqlPrStack::into_pr_stack))
    }

    async fn pull_request_file_diff(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
        path: &str,
    ) -> Result<PrFileDiff> {
        anyhow::ensure!(!path.is_empty(), "pull request file path cannot be empty");
        let mut after = None;
        let mut selected = None;
        let mut base_oid = String::new();
        let mut head_oid = String::new();

        for _ in 0..MAX_PR_FILE_PAGES {
            let response: GraphqlPrFileData = self
                .client
                .graphql(&serde_json::json!({
                    "query": PULL_REQUEST_FILE_QUERY,
                    "variables": {
                        "owner": owner,
                        "repository": repository,
                        "number": number,
                        "after": after,
                    }
                }))
                .await
                .context("locating pull request file through GraphQL")?;
            self.trace_rate("pull request file lookup", &response.rate_limit);
            let pull_request = response
                .repository
                .with_context(|| format!("repository {owner}/{repository} not found"))?
                .pull_request
                .with_context(|| format!("pull request #{number} not found"))?;
            base_oid = pull_request.base_ref_oid;
            head_oid = pull_request.head_ref_oid;
            let GraphqlPagedNodes {
                nodes, page_info, ..
            } = pull_request.files;
            selected = nodes
                .into_iter()
                .flatten()
                .find(|candidate| candidate.path == path);
            if selected.is_some() || !page_info.has_next_page {
                break;
            }
            after = page_info.end_cursor;
            if after.is_none() {
                break;
            }
        }

        let file = selected
            .with_context(|| format!("{path} is not a changed file in pull request #{number}"))?
            .into_pr_file();
        self.pull_request_file_diff_known(owner, repository, &file, &base_oid, &head_oid)
            .await
    }

    async fn pull_request_file_diff_known(
        &self,
        owner: &str,
        repository: &str,
        file: &PrFile,
        base_oid: &str,
        head_oid: &str,
    ) -> Result<PrFileDiff> {
        let path = &file.path;
        let base_expression = format!("{base_oid}:{path}");
        let head_expression = format!("{head_oid}:{path}");
        let metadata_response: GraphqlBlobData = self
            .client
            .graphql(&serde_json::json!({
                "query": PULL_REQUEST_BLOB_METADATA_QUERY,
                "variables": {
                    "owner": owner,
                    "repository": repository,
                    "base": base_expression,
                    "head": head_expression,
                }
            }))
            .await
            .context("loading pull request file metadata through GraphQL")?;
        self.trace_rate("pull request file metadata", &metadata_response.rate_limit);
        let metadata = metadata_response
            .repository
            .with_context(|| format!("repository {owner}/{repository} not found"))?;
        let load_base = blob_text_is_loadable(metadata.base.as_ref());
        let load_head = blob_text_is_loadable(metadata.head.as_ref());
        let content = if load_base || load_head {
            let response: GraphqlBlobData = self
                .client
                .graphql(&serde_json::json!({
                    "query": PULL_REQUEST_BLOB_TEXT_QUERY,
                    "variables": {
                        "owner": owner,
                        "repository": repository,
                        "base": base_expression,
                        "head": head_expression,
                        "loadBase": load_base,
                        "loadHead": load_head,
                    }
                }))
                .await
                .context("loading pull request file text through GraphQL")?;
            self.trace_rate("pull request file text", &response.rate_limit);
            response
                .repository
                .with_context(|| format!("repository {owner}/{repository} not found"))?
        } else {
            GraphqlBlobRepository {
                base: None,
                head: None,
            }
        };
        Ok(build_pr_file_diff(file.clone(), metadata, content))
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

    async fn mutate(&self, query: &str, input: serde_json::Value, operation: &str) -> Result<()> {
        let _: serde_json::Value = self
            .client
            .graphql(&serde_json::json!({
                "query": query,
                "variables": { "input": input },
            }))
            .await
            .with_context(|| format!("{operation} through GitHub GraphQL"))?;
        Ok(())
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

fn blob_text_is_loadable(blob: Option<&GraphqlBlob>) -> bool {
    blob.is_some_and(|blob| {
        blob.typename == "Blob"
            && blob.is_binary != Some(true)
            && blob.is_truncated != Some(true)
            && blob
                .byte_size
                .is_none_or(|bytes| bytes <= MAX_PR_FILE_TEXT_BYTES)
    })
}

fn build_pr_file_diff(
    file: PrFile,
    metadata: GraphqlBlobRepository,
    content: GraphqlBlobRepository,
) -> PrFileDiff {
    let base_expected = file.change_type != "added";
    let head_expected = file.change_type != "deleted";
    let original = if base_expected {
        content.base.and_then(|blob| blob.text)
    } else {
        Some(String::new())
    };
    let modified = if head_expected {
        content.head.and_then(|blob| blob.text)
    } else {
        Some(String::new())
    };
    let mut notes = Vec::new();
    let mut binary = false;
    let mut truncated = false;

    for (side, expected, blob, text) in [
        (
            "base",
            base_expected,
            metadata.base.as_ref(),
            original.as_ref(),
        ),
        (
            "changed",
            head_expected,
            metadata.head.as_ref(),
            modified.as_ref(),
        ),
    ] {
        if !expected {
            continue;
        }
        let Some(blob) = blob else {
            truncated = true;
            notes.push(if file.change_type == "renamed" && side == "base" {
                "GitHub did not expose the file's previous path, so the base side of this rename is unavailable."
                    .to_string()
            } else {
                format!("The {side} file is unavailable at this pull request's commit.")
            });
            continue;
        };
        if blob.typename != "Blob" {
            truncated = true;
            notes.push(format!(
                "The {side} object is a {} rather than a text file.",
                blob.typename.to_ascii_lowercase()
            ));
        } else if blob.is_binary == Some(true) {
            binary = true;
            notes.push(format!("The {side} file is binary."));
        } else if blob.is_truncated == Some(true) {
            truncated = true;
            notes.push(format!("GitHub truncated the {side} file."));
        } else if blob
            .byte_size
            .is_some_and(|bytes| bytes > MAX_PR_FILE_TEXT_BYTES)
        {
            truncated = true;
            notes.push(format!(
                "The {side} file exceeds the {} MB preview limit.",
                MAX_PR_FILE_TEXT_BYTES / 1_000_000
            ));
        } else if text.is_none() {
            truncated = true;
            notes.push(format!("GitHub did not return text for the {side} file."));
        }
    }

    PrFileDiff {
        path: file.path,
        change_type: file.change_type,
        original,
        modified,
        original_bytes: metadata.base.and_then(|blob| blob.byte_size),
        modified_bytes: metadata.head.and_then(|blob| blob.byte_size),
        binary,
        truncated,
        notice: notes.join(" "),
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
                    details_url: check.details_url,
                    started_at: check.started_at,
                    completed_at: check.completed_at,
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
            head_sha: self.head_ref_oid,
            checks,
            reviews,
            trouve_review: None,
            author: self.author.map(|author| author.login).unwrap_or_default(),
            requested_reviewers,
            comments,
            last_comment_at,
            mergeable: match self.mergeable.as_str() {
                "MERGEABLE" => Some(true),
                "CONFLICTING" => Some(false),
                _ => None,
            },
            merge_state_status: self
                .merge_state_status
                .map(|status| status.to_ascii_lowercase()),
            merged_at: self.merged_at,
        }
    }
}

impl GraphqlPrDetailNode {
    #[allow(clippy::too_many_arguments)]
    fn into_pr_detail(
        self,
        host: &str,
        viewer: String,
        repository: GraphqlPrDetailRepository,
        comments: Vec<GraphqlDetailComment>,
        threads: Vec<GraphqlDetailReviewThread>,
        reviews: Vec<GraphqlDetailReview>,
        commits: Vec<GraphqlDetailPullRequestCommit>,
        files: Vec<GraphqlDetailFile>,
        commit_count: u64,
        stack: Option<PrStack>,
        truncated: bool,
    ) -> Result<PrDetail> {
        let GraphqlPrDetailNode {
            summary,
            base_ref_oid,
            body,
            viewer_subscription,
            reaction_groups,
            created_at,
            updated_at,
            additions,
            deletions,
            changed_files,
            review_decision,
            locked,
            active_lock_reason,
            maintainer_can_modify,
            viewer_can_update,
            viewer_can_close,
            viewer_can_reopen,
            viewer_can_assign,
            viewer_can_label,
            viewer_can_merge_as_admin,
            viewer_can_update_branch,
            viewer_can_enable_auto_merge,
            viewer_can_disable_auto_merge,
            viewer_did_author,
            is_merge_queue_enabled,
            merge_queue_entry,
            auto_merge_request,
            labels,
            assignees,
            milestone,
            detail_review_requests,
            connections: _,
        } = self;
        let id = summary.id.clone();
        let info = summary.into_pr_info(host);
        let mut merge_methods = Vec::new();
        if repository.merge_commit_allowed {
            merge_methods.push("merge".to_string());
        }
        if repository.squash_merge_allowed {
            merge_methods.push("squash".to_string());
        }
        if repository.rebase_merge_allowed {
            merge_methods.push("rebase".to_string());
        }
        let default_merge_method = repository.viewer_default_merge_method.to_ascii_lowercase();
        Ok(PrDetail {
            info,
            base_sha: Some(base_ref_oid),
            id,
            viewer,
            body,
            reactions: reaction_groups
                .into_iter()
                .map(GraphqlReactionGroup::into_pr_reaction)
                .collect(),
            viewer_subscription: viewer_subscription.to_ascii_lowercase(),
            created_at,
            updated_at,
            additions,
            deletions,
            changed_files,
            commit_count,
            review_decision: review_decision
                .map(|decision| decision.to_ascii_lowercase())
                .unwrap_or_default(),
            locked,
            active_lock_reason: active_lock_reason
                .map(|reason| reason.to_ascii_lowercase())
                .unwrap_or_default(),
            maintainer_can_modify,
            capabilities: PrCapabilities {
                can_update: viewer_can_update,
                can_close: viewer_can_close,
                can_reopen: viewer_can_reopen,
                can_assign: viewer_can_assign,
                can_label: viewer_can_label,
                can_merge_as_admin: viewer_can_merge_as_admin,
                can_update_branch: viewer_can_update_branch,
                can_enable_auto_merge: viewer_can_enable_auto_merge,
                can_disable_auto_merge: viewer_can_disable_auto_merge,
                did_author: viewer_did_author,
            },
            merge_methods,
            default_merge_method,
            auto_merge_allowed: repository.auto_merge_allowed,
            labels: labels
                .nodes
                .into_iter()
                .flatten()
                .map(GraphqlDetailLabel::into_pr_label)
                .collect(),
            available_labels: repository
                .labels
                .nodes
                .into_iter()
                .flatten()
                .map(GraphqlDetailLabel::into_pr_label)
                .collect(),
            assignees: assignees
                .nodes
                .into_iter()
                .flatten()
                .map(|actor| actor.into_pr_actor("user"))
                .collect(),
            assignable_users: repository
                .assignable_users
                .nodes
                .into_iter()
                .flatten()
                .map(|actor| actor.into_pr_actor("user"))
                .collect(),
            milestone: milestone.map(GraphqlDetailMilestone::into_pr_milestone),
            available_milestones: repository
                .milestones
                .nodes
                .into_iter()
                .flatten()
                .map(GraphqlDetailMilestone::into_pr_milestone)
                .collect(),
            review_requests: detail_review_requests
                .nodes
                .into_iter()
                .flatten()
                .filter_map(|request| request.requested_reviewer)
                .map(GraphqlRequestedReviewer::into_pr_actor)
                .collect(),
            reviews: reviews
                .into_iter()
                .map(GraphqlDetailReview::into_pr_review)
                .collect(),
            comments: comments
                .into_iter()
                .map(GraphqlDetailComment::into_pr_comment)
                .collect(),
            review_threads: threads
                .into_iter()
                .map(GraphqlDetailReviewThread::into_pr_thread)
                .collect(),
            commits: commits
                .into_iter()
                .map(GraphqlDetailPullRequestCommit::into_pr_commit)
                .collect(),
            files: files
                .into_iter()
                .map(GraphqlDetailFile::into_pr_file)
                .collect(),
            merge_queue: PrMergeQueueStatus {
                enabled: is_merge_queue_enabled,
                entry: merge_queue_entry.map(GraphqlMergeQueueEntry::into_pr_entry),
            },
            auto_merge: auto_merge_request.map(GraphqlAutoMerge::into_pr_auto_merge),
            stack,
            truncated,
        })
    }
}

impl GraphqlActor {
    fn into_pr_actor(self, fallback_kind: &str) -> PrActor {
        let kind = if self.typename.is_empty() {
            fallback_kind.to_string()
        } else {
            self.typename.to_ascii_lowercase()
        };
        PrActor {
            id: self.id,
            login: self.login,
            name: self.name.unwrap_or_default(),
            kind,
            avatar_url: self.avatar_url,
            url: self.url,
        }
    }
}

impl GraphqlRequestedReviewer {
    fn into_pr_actor(self) -> PrActor {
        let kind = self.typename.to_ascii_lowercase();
        let login = self
            .login
            .or_else(|| self.slug.clone())
            .or_else(|| self.name.clone())
            .unwrap_or_default();
        PrActor {
            id: self.id,
            login,
            name: self.name.unwrap_or_default(),
            kind: if kind.is_empty() {
                "unknown".into()
            } else {
                kind
            },
            avatar_url: self.avatar_url,
            url: self.url,
        }
    }
}

impl GraphqlDetailLabel {
    fn into_pr_label(self) -> PrLabel {
        PrLabel {
            id: self.id,
            name: self.name,
            color: self.color,
            description: self.description.unwrap_or_default(),
        }
    }
}

impl GraphqlDetailMilestone {
    fn into_pr_milestone(self) -> PrMilestone {
        PrMilestone {
            id: self.id,
            number: self.number,
            title: self.title,
            state: self.state.to_ascii_lowercase(),
            url: self.url,
        }
    }
}

impl GraphqlMergeQueueEntry {
    fn into_pr_entry(self) -> PrMergeQueueEntry {
        PrMergeQueueEntry {
            id: self.id,
            position: self.position,
            state: self.state.to_ascii_lowercase(),
            enqueued_at: self.enqueued_at,
            estimated_time_to_merge: self.estimated_time_to_merge,
        }
    }
}

impl GraphqlAutoMerge {
    fn into_pr_auto_merge(self) -> PrAutoMerge {
        PrAutoMerge {
            method: self.merge_method.to_ascii_lowercase(),
            enabled_at: self.enabled_at,
            enabled_by: self.enabled_by.map(|actor| actor.into_pr_actor("user")),
            commit_title: self.commit_headline.unwrap_or_default(),
            commit_message: self.commit_body.unwrap_or_default(),
        }
    }
}

impl GraphqlDetailComment {
    fn into_pr_comment(self) -> PrComment {
        PrComment {
            id: self.id,
            database_id: self.database_id,
            body: self.body,
            url: self.url,
            author: self.author.map(|actor| actor.into_pr_actor("user")),
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_edited_at: self.last_edited_at,
            viewer_can_update: self.viewer_can_update,
            viewer_can_delete: self.viewer_can_delete,
            viewer_did_author: self.viewer_did_author,
            reactions: self
                .reaction_groups
                .into_iter()
                .map(GraphqlReactionGroup::into_pr_reaction)
                .collect(),
            path: self.path,
            line: self.line,
            diff_hunk: self.diff_hunk,
        }
    }
}

impl GraphqlReactionGroup {
    fn into_pr_reaction(self) -> PrReactionSummary {
        PrReactionSummary {
            content: self.content.to_ascii_lowercase(),
            count: self.users.total_count,
            viewer_has_reacted: self.viewer_has_reacted,
        }
    }
}

impl GraphqlDetailReviewThread {
    fn into_pr_thread(self) -> PrReviewThread {
        PrReviewThread {
            id: self.id,
            path: self.path,
            line: self.line,
            start_line: self.start_line,
            diff_side: self.diff_side.to_ascii_lowercase(),
            is_outdated: self.is_outdated,
            is_resolved: self.is_resolved,
            viewer_can_reply: self.viewer_can_reply,
            viewer_can_resolve: self.viewer_can_resolve,
            viewer_can_unresolve: self.viewer_can_unresolve,
            comments: self
                .comments
                .nodes
                .into_iter()
                .flatten()
                .map(GraphqlDetailComment::into_pr_comment)
                .collect(),
        }
    }
}

impl GraphqlDetailReview {
    fn into_pr_review(self) -> PrReviewDetail {
        PrReviewDetail {
            id: self.id,
            author: self.author.map(|actor| actor.into_pr_actor("user")),
            state: self.state.to_ascii_lowercase(),
            body: self.body,
            url: self.url,
            submitted_at: self.submitted_at,
            commit_oid: self.commit.map(|commit| commit.oid).unwrap_or_default(),
            viewer_can_update: self.viewer_can_update,
            viewer_can_delete: self.viewer_can_delete,
            viewer_did_author: self.viewer_did_author,
        }
    }
}

impl GraphqlDetailPullRequestCommit {
    fn into_pr_commit(self) -> PrCommit {
        let actor = self.commit.author.and_then(|author| {
            author
                .user
                .map(|actor| actor.into_pr_actor("user"))
                .or_else(|| {
                    author.name.map(|name| PrActor {
                        id: String::new(),
                        login: String::new(),
                        name,
                        kind: "unknown".into(),
                        avatar_url: String::new(),
                        url: String::new(),
                    })
                })
        });
        PrCommit {
            oid: self.commit.oid,
            abbreviated_oid: self.commit.abbreviated_oid,
            message_headline: self.commit.message_headline,
            message_body: self.commit.message_body,
            committed_at: self.commit.committed_date,
            author: actor,
            url: self.commit.url,
        }
    }
}

impl GraphqlDetailFile {
    fn into_pr_file(self) -> PrFile {
        PrFile {
            path: self.path,
            additions: self.additions,
            deletions: self.deletions,
            change_type: self.change_type.to_ascii_lowercase(),
            viewer_viewed_state: self.viewer_viewed_state.to_ascii_lowercase(),
        }
    }
}

impl GraphqlPrStack {
    fn into_pr_stack(self) -> PrStack {
        PrStack {
            id: self.id,
            number: self.number,
            size: self.size,
            base: self.base_ref_name,
            entries: self
                .entries
                .nodes
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let pull_request = entry.pull_request?;
                    Some(PrStackEntry {
                        position: entry.position,
                        number: pull_request.number,
                        title: pull_request.title,
                        url: pull_request.url,
                        state: pull_request.state.to_ascii_lowercase(),
                        draft: pull_request.is_draft,
                        base: pull_request.base_ref_name,
                        head: pull_request.head_ref_name,
                        review_decision: pull_request
                            .review_decision
                            .map(|decision| decision.to_ascii_lowercase())
                            .unwrap_or_default(),
                        merge_state_status: pull_request
                            .merge_state_status
                            .map(|status| status.to_ascii_lowercase())
                            .unwrap_or_default(),
                    })
                })
                .collect(),
        }
    }
}

fn consume_detail_page<T>(
    page: Option<GraphqlPagedNodes<T>>,
    items: &mut Vec<T>,
    after: &mut Option<String>,
    active: &mut bool,
) {
    let Some(page) = page else {
        *active = false;
        return;
    };
    items.extend(page.nodes.into_iter().flatten());
    *active = page.page_info.has_next_page && page.page_info.end_cursor.is_some();
    *after = (*active).then_some(page.page_info.end_cursor).flatten();
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
    probes: &mut BTreeMap<String, DashboardFingerprint>,
) {
    if !cursor.active {
        return;
    }
    let Some(page) = page else {
        cursor.active = false;
        return;
    };
    probes.extend(
        page.nodes
            .into_iter()
            .flatten()
            .map(GraphqlDashboardProbe::into_entry),
    );
    cursor.pages += 1;
    cursor.active = page.page_info.has_next_page
        && cursor.pages < DASHBOARD_MAX_PR_PAGES
        && page.page_info.end_cursor.is_some();
    cursor.after = cursor.active.then_some(page.page_info.end_cursor).flatten();
}

fn operation_with_pr_fields(operation: &str) -> String {
    format!("{operation}\n{PULL_REQUEST_FIELDS}")
}

fn operation_with_pr_detail_fields(operation: &str) -> String {
    format!("{operation}\n{PULL_REQUEST_DETAIL_CONNECTIONS}\n{PULL_REQUEST_FIELDS}")
}

fn operation_with_pr_detail_connections(operation: &str) -> String {
    format!("{operation}\n{PULL_REQUEST_DETAIL_CONNECTIONS}")
}

fn graphql_base_uri(host: &str) -> Option<String> {
    (host != GITHUB_COM).then(|| format!("https://{host}/api"))
}

fn insert_optional<T: serde::Serialize>(
    input: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    value: Option<&T>,
) {
    if let Some(value) = value {
        input.insert(field.into(), serde_json::json!(value));
    }
}

fn insert_nonempty(
    input: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    value: &str,
) {
    if !value.trim().is_empty() {
        input.insert(field.into(), serde_json::json!(value));
    }
}

fn github_merge_method(method: &str) -> Result<&'static str> {
    match method.to_ascii_lowercase().as_str() {
        "" | "merge" => Ok("merge"),
        "squash" => Ok("squash"),
        "rebase" => Ok("rebase"),
        _ => anyhow::bail!("unsupported merge method: {method}"),
    }
}

fn github_review_event(event: &str) -> Result<&'static str> {
    match event.to_ascii_lowercase().as_str() {
        "approve" => Ok("APPROVE"),
        "request_changes" | "changes_requested" => Ok("REQUEST_CHANGES"),
        "comment" => Ok("COMMENT"),
        _ => anyhow::bail!("unsupported review event: {event}"),
    }
}

fn github_diff_side(side: &str) -> Result<&'static str> {
    match side.to_ascii_lowercase().as_str() {
        "left" => Ok("LEFT"),
        "right" => Ok("RIGHT"),
        _ => anyhow::bail!("unsupported review-comment side: {side}"),
    }
}

fn github_lock_reason(reason: &str) -> Result<&'static str> {
    match reason.to_ascii_lowercase().as_str() {
        "off_topic" => Ok("OFF_TOPIC"),
        "too_heated" => Ok("TOO_HEATED"),
        "resolved" => Ok("RESOLVED"),
        "spam" => Ok("SPAM"),
        _ => anyhow::bail!("unsupported conversation lock reason: {reason}"),
    }
}

fn github_reaction_content(content: &str) -> Result<&'static str> {
    match content.to_ascii_uppercase().as_str() {
        "THUMBS_UP" => Ok("THUMBS_UP"),
        "THUMBS_DOWN" => Ok("THUMBS_DOWN"),
        "LAUGH" => Ok("LAUGH"),
        "HOORAY" => Ok("HOORAY"),
        "CONFUSED" => Ok("CONFUSED"),
        "HEART" => Ok("HEART"),
        "ROCKET" => Ok("ROCKET"),
        "EYES" => Ok("EYES"),
        _ => anyhow::bail!("unsupported reaction: {content}"),
    }
}

fn github_subscription_state(state: &str) -> Result<&'static str> {
    match state.to_ascii_lowercase().as_str() {
        "subscribed" => Ok("SUBSCRIBED"),
        "unsubscribed" => Ok("UNSUBSCRIBED"),
        "ignored" => Ok("IGNORED"),
        _ => anyhow::bail!("unsupported pull request subscription state: {state}"),
    }
}

fn ensure_comment_id(detail: &PrDetail, id: &str, kind: &PrCommentKind) -> Result<()> {
    let found = match kind {
        PrCommentKind::Issue => detail.comments.iter().any(|comment| comment.id == id),
        PrCommentKind::Review => detail
            .review_threads
            .iter()
            .flat_map(|thread| &thread.comments)
            .any(|comment| comment.id == id),
    };
    anyhow::ensure!(
        found,
        "comment does not belong to the selected pull request"
    );
    Ok(())
}

fn review_by_id<'a>(detail: &'a PrDetail, id: &str) -> Result<&'a PrReviewDetail> {
    detail
        .reviews
        .iter()
        .find(|review| review.id == id)
        .context("review does not belong to the selected pull request")
}

fn ensure_ids_exist<'a>(
    requested: &[String],
    available: impl Iterator<Item = &'a str>,
    kind: &str,
) -> Result<()> {
    let available = available.collect::<HashSet<_>>();
    for id in requested {
        anyhow::ensure!(
            available.contains(id.as_str()),
            "{kind} does not belong to the selected repository"
        );
    }
    Ok(())
}

fn pr_subject_ids(detail: &PrDetail) -> HashSet<&str> {
    let mut ids = HashSet::from([detail.id.as_str()]);
    ids.extend(detail.comments.iter().map(|comment| comment.id.as_str()));
    ids.extend(detail.reviews.iter().map(|review| review.id.as_str()));
    ids.extend(
        detail
            .review_threads
            .iter()
            .flat_map(|thread| thread.comments.iter())
            .map(|comment| comment.id.as_str()),
    );
    ids
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
        cache: &mut GitHubDashboardCache,
    ) -> Result<(String, Vec<PrInfo>)> {
        self.graphql.dashboard_prs(merged_since, cache).await
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

    /// Full, lazily loaded PR-page state for one selected pull request.
    pub async fn pr_detail(
        &self,
        number: u64,
        sections: &HashSet<PrDetailSection>,
        existing: Option<PrDetail>,
    ) -> Result<PrDetail> {
        self.graphql
            .pull_request_detail(&self.owner, &self.repo, number, sections, existing)
            .await
    }

    /// Load one changed file's immutable base/head text without requesting the
    /// pull request's aggregate patch. Membership is resolved from GitHub
    /// before either repository object expression is evaluated.
    pub async fn pr_file_diff(&self, number: u64, path: &str) -> Result<PrFileDiff> {
        self.graphql
            .pull_request_file_diff(&self.owner, &self.repo, number, path)
            .await
    }

    /// Load immutable content for a file already returned by a cached PR
    /// detail snapshot. This deliberately skips GitHub's changed-files
    /// connection: membership and both object ids were established by the
    /// selected PR cache.
    pub async fn pr_file_diff_known(
        &self,
        file: &PrFile,
        base_oid: &str,
        head_oid: &str,
    ) -> Result<PrFileDiff> {
        self.graphql
            .pull_request_file_diff_known(&self.owner, &self.repo, file, base_oid, head_oid)
            .await
    }

    /// Apply one typed collaboration action to a selected pull request. The
    /// engine supplies a head-keyed cached detail snapshot containing the
    /// sections required to validate this action, so mutations do not need a
    /// full detail read both before and after every write.
    pub async fn act_on_pr(&self, detail: &PrDetail, action: &PrActionRequest) -> Result<()> {
        self.validate_pr_action(detail, action)?;
        self.execute_pr_action(detail, action).await?;
        Ok(())
    }

    fn validate_pr_action(&self, detail: &PrDetail, action: &PrActionRequest) -> Result<()> {
        match action {
            PrActionRequest::Update { .. } => {
                anyhow::ensure!(
                    detail.capabilities.can_update,
                    "GitHub does not permit updating this pull request"
                );
            }
            PrActionRequest::SetState { state } => match state.as_str() {
                "draft" | "ready" => anyhow::ensure!(
                    detail.capabilities.can_update,
                    "GitHub does not permit changing this pull request's review state"
                ),
                "close" => anyhow::ensure!(
                    detail.capabilities.can_close,
                    "GitHub does not permit closing this pull request"
                ),
                "reopen" => anyhow::ensure!(
                    detail.capabilities.can_reopen,
                    "GitHub does not permit reopening this pull request"
                ),
                _ => anyhow::bail!("unsupported pull request state action: {state}"),
            },
            PrActionRequest::RequestReviewers {
                users,
                bots,
                teams,
                replace,
            } => {
                anyhow::ensure!(
                    *replace || !users.is_empty() || !bots.is_empty() || !teams.is_empty(),
                    "select at least one reviewer"
                );
            }
            PrActionRequest::SubmitReview { event, .. } => {
                github_review_event(event)?;
            }
            PrActionRequest::UpdateReview { id, .. } => {
                anyhow::ensure!(
                    review_by_id(detail, id)?.viewer_can_update,
                    "GitHub does not permit updating this review"
                );
            }
            PrActionRequest::DeleteReview { id } => {
                anyhow::ensure!(
                    review_by_id(detail, id)?.viewer_can_delete,
                    "GitHub does not permit deleting this review"
                );
            }
            PrActionRequest::DismissReview { id, message } => {
                review_by_id(detail, id)?;
                anyhow::ensure!(
                    detail.capabilities.can_update,
                    "GitHub does not permit dismissing reviews"
                );
                anyhow::ensure!(
                    !message.trim().is_empty(),
                    "review dismissal message cannot be empty"
                );
            }
            PrActionRequest::AddComment { body } => {
                anyhow::ensure!(!body.trim().is_empty(), "comment body cannot be empty");
            }
            PrActionRequest::UpdateComment { id, kind, body } => {
                anyhow::ensure!(!body.trim().is_empty(), "comment body cannot be empty");
                ensure_comment_id(detail, id, kind)?;
            }
            PrActionRequest::DeleteComment { id, kind } => ensure_comment_id(detail, id, kind)?,
            PrActionRequest::ReplyReviewThread { thread_id, body } => {
                anyhow::ensure!(!body.trim().is_empty(), "comment body cannot be empty");
                anyhow::ensure!(
                    detail
                        .review_threads
                        .iter()
                        .any(|thread| thread.id == *thread_id),
                    "review thread does not belong to the selected pull request"
                );
            }
            PrActionRequest::ResolveReviewThread { thread_id, .. } => {
                anyhow::ensure!(
                    detail
                        .review_threads
                        .iter()
                        .any(|thread| thread.id == *thread_id),
                    "review thread does not belong to the selected pull request"
                );
            }
            PrActionRequest::AddReviewThread {
                body,
                side,
                start_side,
                ..
            } => {
                anyhow::ensure!(!body.trim().is_empty(), "comment body cannot be empty");
                github_diff_side(side)?;
                if let Some(start_side) = start_side {
                    github_diff_side(start_side)?;
                }
            }
            PrActionRequest::SetFileViewed { path, .. } => {
                anyhow::ensure!(
                    detail.files.iter().any(|file| file.path == *path),
                    "file does not belong to the selected pull request"
                );
            }
            PrActionRequest::UpdateBranch { .. } => {
                anyhow::ensure!(
                    detail.capabilities.can_update_branch,
                    "GitHub does not permit updating this pull request branch"
                );
            }
            PrActionRequest::Merge { method, .. } => {
                anyhow::ensure!(
                    detail.info.state == "open",
                    "only an open pull request can be merged"
                );
                let method = github_merge_method(method)?;
                anyhow::ensure!(
                    detail.merge_methods.iter().any(|enabled| enabled == method),
                    "the repository has disabled the selected merge method"
                );
            }
            PrActionRequest::SetAutoMerge {
                enabled, method, ..
            } => {
                if *enabled {
                    anyhow::ensure!(
                        detail.auto_merge_allowed,
                        "auto-merge is disabled for this repository"
                    );
                    let method = github_merge_method(method)?;
                    anyhow::ensure!(
                        detail.merge_methods.iter().any(|enabled| enabled == method),
                        "the repository has disabled the selected merge method"
                    );
                }
            }
            PrActionRequest::SetMergeQueue { enabled, .. } => {
                anyhow::ensure!(
                    detail.merge_queue.enabled,
                    "this branch does not use a merge queue"
                );
                if !enabled {
                    anyhow::ensure!(
                        detail.merge_queue.entry.is_some(),
                        "pull request is not in the merge queue"
                    );
                }
            }
            PrActionRequest::SetLabels { label_ids } => {
                anyhow::ensure!(
                    detail.capabilities.can_label,
                    "GitHub does not permit changing labels"
                );
                ensure_ids_exist(
                    label_ids,
                    detail
                        .available_labels
                        .iter()
                        .map(|label| label.id.as_str()),
                    "label",
                )?;
            }
            PrActionRequest::SetAssignees { assignee_ids } => {
                anyhow::ensure!(
                    detail.capabilities.can_assign,
                    "GitHub does not permit changing assignees"
                );
                ensure_ids_exist(
                    assignee_ids,
                    detail
                        .assignable_users
                        .iter()
                        .map(|actor| actor.id.as_str()),
                    "assignee",
                )?;
            }
            PrActionRequest::SetMilestone { milestone_id } => {
                if let Some(id) = milestone_id {
                    anyhow::ensure!(
                        detail
                            .available_milestones
                            .iter()
                            .any(|milestone| milestone.id == *id),
                        "milestone does not belong to the selected repository"
                    );
                }
            }
            PrActionRequest::SetLock { reason, .. } => {
                if let Some(reason) = reason {
                    github_lock_reason(reason)?;
                }
            }
            PrActionRequest::SetSubscription { state } => {
                github_subscription_state(state)?;
            }
            PrActionRequest::AddReaction {
                subject_id,
                content,
            }
            | PrActionRequest::RemoveReaction {
                subject_id,
                content,
            } => {
                anyhow::ensure!(
                    pr_subject_ids(detail).contains(subject_id.as_str()),
                    "reaction target does not belong to the selected pull request"
                );
                github_reaction_content(content)?;
            }
        }
        Ok(())
    }

    async fn execute_pr_action(&self, detail: &PrDetail, action: &PrActionRequest) -> Result<()> {
        use serde_json::{Map, Value, json};

        let (query, input, operation) = match action {
            PrActionRequest::Update {
                title,
                body,
                base,
                maintainer_can_modify,
            } => {
                let mut input = Map::from_iter([("pullRequestId".into(), json!(detail.id))]);
                insert_optional(&mut input, "title", title.as_ref());
                insert_optional(&mut input, "body", body.as_ref());
                insert_optional(&mut input, "baseRefName", base.as_ref());
                insert_optional(
                    &mut input,
                    "maintainerCanModify",
                    maintainer_can_modify.as_ref(),
                );
                (
                    UPDATE_PULL_REQUEST_MUTATION,
                    Value::Object(input),
                    "updating pull request",
                )
            }
            PrActionRequest::SetState { state } => match state.as_str() {
                "draft" => (
                    CONVERT_PULL_REQUEST_TO_DRAFT_MUTATION,
                    json!({ "pullRequestId": detail.id }),
                    "converting pull request to draft",
                ),
                "ready" => (
                    MARK_PULL_REQUEST_READY_MUTATION,
                    json!({ "pullRequestId": detail.id }),
                    "marking pull request ready for review",
                ),
                "close" => (
                    CLOSE_PULL_REQUEST_MUTATION,
                    json!({ "pullRequestId": detail.id }),
                    "closing pull request",
                ),
                "reopen" => (
                    REOPEN_PULL_REQUEST_MUTATION,
                    json!({ "pullRequestId": detail.id }),
                    "reopening pull request",
                ),
                _ => unreachable!("state validated before mutation"),
            },
            PrActionRequest::RequestReviewers {
                users,
                bots,
                teams,
                replace,
            } => (
                REQUEST_REVIEWS_MUTATION,
                json!({ "pullRequestId": detail.id, "userLogins": users, "botLogins": bots, "teamSlugs": teams, "union": !replace }),
                "requesting pull request reviewers",
            ),
            PrActionRequest::SubmitReview { event, body } => {
                let pending = detail
                    .reviews
                    .iter()
                    .find(|review| review.state == "pending" && review.viewer_did_author);
                if let Some(review) = pending {
                    (
                        SUBMIT_REVIEW_MUTATION,
                        json!({
                            "pullRequestReviewId": review.id,
                            "event": github_review_event(event)?,
                            "body": body,
                        }),
                        "submitting pending pull request review",
                    )
                } else {
                    (
                        ADD_REVIEW_MUTATION,
                        json!({ "pullRequestId": detail.id, "event": github_review_event(event)?, "body": body }),
                        "submitting pull request review",
                    )
                }
            }
            PrActionRequest::UpdateReview { id, body } => (
                UPDATE_REVIEW_MUTATION,
                json!({ "pullRequestReviewId": id, "body": body }),
                "updating pull request review",
            ),
            PrActionRequest::DeleteReview { id } => (
                DELETE_REVIEW_MUTATION,
                json!({ "pullRequestReviewId": id }),
                "deleting pull request review",
            ),
            PrActionRequest::DismissReview { id, message } => (
                DISMISS_REVIEW_MUTATION,
                json!({ "pullRequestReviewId": id, "message": message }),
                "dismissing pull request review",
            ),
            PrActionRequest::AddComment { body } => (
                ADD_COMMENT_MUTATION,
                json!({ "subjectId": detail.id, "body": body }),
                "adding pull request comment",
            ),
            PrActionRequest::UpdateComment { id, kind, body } => match kind {
                PrCommentKind::Issue => (
                    UPDATE_ISSUE_COMMENT_MUTATION,
                    json!({ "id": id, "body": body }),
                    "updating pull request comment",
                ),
                PrCommentKind::Review => (
                    UPDATE_REVIEW_COMMENT_MUTATION,
                    json!({ "pullRequestReviewCommentId": id, "body": body }),
                    "updating review comment",
                ),
            },
            PrActionRequest::DeleteComment { id, kind } => match kind {
                PrCommentKind::Issue => (
                    DELETE_ISSUE_COMMENT_MUTATION,
                    json!({ "id": id }),
                    "deleting pull request comment",
                ),
                PrCommentKind::Review => (
                    DELETE_REVIEW_COMMENT_MUTATION,
                    json!({ "pullRequestReviewCommentId": id }),
                    "deleting review comment",
                ),
            },
            PrActionRequest::ReplyReviewThread { thread_id, body } => (
                REPLY_REVIEW_THREAD_MUTATION,
                json!({ "pullRequestReviewThreadId": thread_id, "body": body }),
                "replying to review thread",
            ),
            PrActionRequest::ResolveReviewThread {
                thread_id,
                resolved,
            } => {
                if *resolved {
                    (
                        RESOLVE_REVIEW_THREAD_MUTATION,
                        json!({ "threadId": thread_id }),
                        "resolving review thread",
                    )
                } else {
                    (
                        UNRESOLVE_REVIEW_THREAD_MUTATION,
                        json!({ "threadId": thread_id }),
                        "reopening review thread",
                    )
                }
            }
            PrActionRequest::AddReviewThread {
                body,
                path,
                line,
                side,
                start_line,
                start_side,
            } => {
                let mut input = Map::from_iter([
                    ("body".into(), json!(body)),
                    ("path".into(), json!(path)),
                    ("line".into(), json!(line)),
                    ("side".into(), json!(github_diff_side(side)?)),
                ]);
                if let Some(review) = detail
                    .reviews
                    .iter()
                    .find(|review| review.state == "pending" && review.viewer_did_author)
                {
                    input.insert("pullRequestReviewId".into(), json!(review.id));
                } else {
                    input.insert("pullRequestId".into(), json!(detail.id));
                }
                insert_optional(&mut input, "startLine", start_line.as_ref());
                if let Some(start_side) = start_side {
                    input.insert("startSide".into(), json!(github_diff_side(start_side)?));
                }
                (
                    ADD_REVIEW_THREAD_MUTATION,
                    Value::Object(input),
                    "adding review thread",
                )
            }
            PrActionRequest::SetFileViewed { path, viewed } => {
                if *viewed {
                    (
                        MARK_FILE_VIEWED_MUTATION,
                        json!({ "pullRequestId": detail.id, "path": path }),
                        "marking pull request file viewed",
                    )
                } else {
                    (
                        UNMARK_FILE_VIEWED_MUTATION,
                        json!({ "pullRequestId": detail.id, "path": path }),
                        "marking pull request file unviewed",
                    )
                }
            }
            PrActionRequest::UpdateBranch { expected_head_sha } => {
                let mut input = Map::from_iter([("pullRequestId".into(), json!(detail.id))]);
                insert_optional(&mut input, "expectedHeadOid", expected_head_sha.as_ref());
                (
                    UPDATE_PULL_REQUEST_BRANCH_MUTATION,
                    Value::Object(input),
                    "updating pull request branch",
                )
            }
            PrActionRequest::Merge {
                method,
                commit_title,
                commit_message,
                expected_head_sha,
            } => {
                let mut input = Map::from_iter([
                    ("pullRequestId".into(), json!(detail.id)),
                    (
                        "mergeMethod".into(),
                        json!(github_merge_method(method)?.to_ascii_uppercase()),
                    ),
                ]);
                insert_nonempty(&mut input, "commitHeadline", commit_title);
                insert_nonempty(&mut input, "commitBody", commit_message);
                insert_optional(&mut input, "expectedHeadOid", expected_head_sha.as_ref());
                (
                    MERGE_PULL_REQUEST_MUTATION,
                    Value::Object(input),
                    "merging pull request",
                )
            }
            PrActionRequest::SetAutoMerge {
                enabled,
                method,
                commit_title,
                commit_message,
            } => {
                if *enabled {
                    let mut input = Map::from_iter([
                        ("pullRequestId".into(), json!(detail.id)),
                        (
                            "mergeMethod".into(),
                            json!(github_merge_method(method)?.to_ascii_uppercase()),
                        ),
                    ]);
                    insert_nonempty(&mut input, "commitHeadline", commit_title);
                    insert_nonempty(&mut input, "commitBody", commit_message);
                    (
                        ENABLE_AUTO_MERGE_MUTATION,
                        Value::Object(input),
                        "enabling pull request auto-merge",
                    )
                } else {
                    (
                        DISABLE_AUTO_MERGE_MUTATION,
                        json!({ "pullRequestId": detail.id }),
                        "disabling pull request auto-merge",
                    )
                }
            }
            PrActionRequest::SetMergeQueue {
                enabled,
                expected_head_sha,
            } => {
                if *enabled {
                    let mut input = Map::from_iter([("pullRequestId".into(), json!(detail.id))]);
                    insert_optional(&mut input, "expectedHeadOid", expected_head_sha.as_ref());
                    (
                        ENQUEUE_PULL_REQUEST_MUTATION,
                        Value::Object(input),
                        "adding pull request to merge queue",
                    )
                } else {
                    let entry_id = &detail
                        .merge_queue
                        .entry
                        .as_ref()
                        .expect("queue entry validated")
                        .id;
                    (
                        DEQUEUE_PULL_REQUEST_MUTATION,
                        json!({ "id": entry_id }),
                        "removing pull request from merge queue",
                    )
                }
            }
            PrActionRequest::SetLabels { label_ids } => (
                UPDATE_PULL_REQUEST_MUTATION,
                json!({ "pullRequestId": detail.id, "labelIds": label_ids }),
                "updating pull request labels",
            ),
            PrActionRequest::SetAssignees { assignee_ids } => (
                UPDATE_PULL_REQUEST_MUTATION,
                json!({ "pullRequestId": detail.id, "assigneeIds": assignee_ids }),
                "updating pull request assignees",
            ),
            PrActionRequest::SetMilestone { milestone_id } => (
                UPDATE_PULL_REQUEST_MUTATION,
                json!({ "pullRequestId": detail.id, "milestoneId": milestone_id }),
                "updating pull request milestone",
            ),
            PrActionRequest::SetLock { locked, reason } => {
                if *locked {
                    let mut input = Map::from_iter([("lockableId".into(), json!(detail.id))]);
                    if let Some(reason) = reason {
                        input.insert("lockReason".into(), json!(github_lock_reason(reason)?));
                    }
                    (
                        LOCK_LOCKABLE_MUTATION,
                        Value::Object(input),
                        "locking pull request conversation",
                    )
                } else {
                    (
                        UNLOCK_LOCKABLE_MUTATION,
                        json!({ "lockableId": detail.id }),
                        "unlocking pull request conversation",
                    )
                }
            }
            PrActionRequest::SetSubscription { state } => (
                UPDATE_SUBSCRIPTION_MUTATION,
                json!({ "subscribableId": detail.id, "state": github_subscription_state(state)? }),
                "updating pull request notification subscription",
            ),
            PrActionRequest::AddReaction {
                subject_id,
                content,
            } => (
                ADD_REACTION_MUTATION,
                json!({ "subjectId": subject_id, "content": github_reaction_content(content)? }),
                "adding reaction",
            ),
            PrActionRequest::RemoveReaction {
                subject_id,
                content,
            } => (
                REMOVE_REACTION_MUTATION,
                json!({ "subjectId": subject_id, "content": github_reaction_content(content)? }),
                "removing reaction",
            ),
        };
        self.graphql.mutate(query, input, operation).await
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
                        details_url: run.details_url.map(|url| url.to_string()),
                        started_at: run.started_at,
                        completed_at: run.completed_at,
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
            head_sha: Some(pr.head.sha),
            checks,
            reviews,
            trouve_review: None,
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
            // REST does not expose GraphQL's detailed merge state. The next
            // shared-account or session refresh fills it in.
            merge_state_status: None,
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
            "id": "PR_42",
            "repository": { "nameWithOwner": "acme/widgets" },
            "headRepository": { "nameWithOwner": "acme/widgets" },
            "number": 42,
            "url": "https://github.example.com/acme/widgets/pull/42",
            "title": "Ship the widgets",
            "state": "OPEN",
            "isDraft": false,
            "baseRefName": "main",
            "headRefName": "ship-widgets",
            "headRefOid": "abc123",
            "author": { "login": "alice" },
            "mergeable": "CONFLICTING",
            "mergeStateStatus": "BLOCKED",
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
        assert_eq!(info.head_sha.as_deref(), Some("abc123"));
        assert_eq!(info.author, "alice");
        assert_eq!(info.requested_reviewers, ["bob"]);
        assert_eq!(info.comments, 4);
        assert_eq!(
            info.last_comment_at,
            Some("2026-07-20T11:00:00Z".parse().unwrap())
        );
        assert_eq!(info.mergeable, Some(false));
        assert_eq!(info.merge_state_status.as_deref(), Some("blocked"));
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
        let mut probes = BTreeMap::new();
        for page in 1..=DASHBOARD_MAX_PR_PAGES {
            consume_search_page(
                Some(GraphqlSearchConnection {
                    nodes: vec![Some(GraphqlDashboardProbe {
                        id: format!("pr-{page}"),
                        updated_at: "2026-07-20T12:00:00Z".parse().unwrap(),
                        head_ref_oid: Some(format!("sha-{page}")),
                        mergeable: "MERGEABLE".into(),
                        commits: GraphqlProbeCommits { nodes: Vec::new() },
                    })],
                    page_info: GraphqlPageInfo {
                        has_next_page: true,
                        end_cursor: Some(format!("cursor-{page}")),
                    },
                }),
                &mut cursor,
                &mut probes,
            );
        }

        assert_eq!(probes.len(), DASHBOARD_MAX_PR_PAGES);
        assert!(!cursor.active);
        assert_eq!(cursor.pages, DASHBOARD_MAX_PR_PAGES);
    }

    #[test]
    fn detail_continuation_query_omits_repeated_overview_fields() {
        let query = operation_with_pr_detail_connections(PULL_REQUEST_DETAIL_PAGE_QUERY);
        assert!(query.contains("fragment TrouvePullRequestDetailConnections"));
        assert!(query.contains("detailReviewThreads: reviewThreads"));
        assert!(!query.contains("TrouvePullRequestFields"));
        assert!(!query.contains("viewer { login }"));
        assert!(!query.contains("mergeCommitAllowed"));
        assert!(!query.contains("assignableUsers"));
    }

    #[test]
    fn dashboard_cache_coalesces_background_and_concurrent_forced_refreshes() {
        let completed = Instant::now();
        let freshness = Duration::from_secs(25);
        let cache = GitHubDashboardCache {
            last_successful_refresh: Some(completed),
            ..Default::default()
        };

        assert!(!cache.should_refresh(
            false,
            completed,
            completed + Duration::from_secs(24),
            freshness,
        ));
        assert!(cache.should_refresh(false, completed, completed + freshness, freshness,));
        assert!(!cache.should_refresh(true, completed, completed + freshness, freshness,));
        assert!(cache.should_refresh(
            true,
            completed + Duration::from_nanos(1),
            completed + freshness,
            freshness,
        ));
    }

    #[test]
    fn dashboard_cache_reuses_unchanged_probes_and_resets_for_viewer() {
        let fingerprint = |check_state: Option<&str>| DashboardFingerprint {
            updated_at: "2026-07-20T12:00:00Z".parse().unwrap(),
            head_ref_oid: Some("abc123".into()),
            mergeable: "MERGEABLE".into(),
            check_state: check_state.map(str::to_string),
        };
        let pending = fingerprint(Some("PENDING"));
        let mut cache = GitHubDashboardCache::default();
        cache.begin_viewer("alice");
        assert!(cache.needs_detail_refresh("PR_42", &pending));

        cache.entries.insert(
            "PR_42".into(),
            CachedDashboardPullRequest {
                fingerprint: pending.clone(),
                pull_request: PrInfo {
                    host: "github.com".into(),
                    repository: "acme/widgets".into(),
                    workspace_id: String::new(),
                    number: 42,
                    url: "https://github.com/acme/widgets/pull/42".into(),
                    title: "Ship the widgets".into(),
                    state: "open".into(),
                    draft: false,
                    base: "main".into(),
                    head: "ship-widgets".into(),
                    head_sha: pending.head_ref_oid.clone(),
                    checks: Vec::new(),
                    reviews: Vec::new(),
                    trouve_review: None,
                    author: "alice".into(),
                    requested_reviewers: Vec::new(),
                    comments: 0,
                    last_comment_at: None,
                    mergeable: Some(true),
                    merge_state_status: None,
                    merged_at: None,
                },
            },
        );

        assert!(!cache.needs_detail_refresh("PR_42", &pending));
        assert!(cache.needs_detail_refresh("PR_42", &fingerprint(Some("SUCCESS"))));
        cache.begin_viewer("alice");
        assert_eq!(cache.entries.len(), 1);
        cache.begin_viewer("bob");
        assert!(cache.entries.is_empty());

        let snapshot = GithubPrList {
            viewer: "bob".into(),
            host: "github.com".into(),
            prs: Vec::new(),
        };
        let serialized = cache.unpublished_snapshot(&snapshot).unwrap().unwrap();
        cache.mark_snapshot_published(serialized);
        assert!(cache.unpublished_snapshot(&snapshot).unwrap().is_none());

        let changed = GithubPrList {
            viewer: "carol".into(),
            ..snapshot
        };
        assert!(cache.unpublished_snapshot(&changed).unwrap().is_some());
    }

    #[test]
    fn pr_file_diff_preserves_added_file_semantics_and_text() {
        let file = PrFile {
            path: "src/new.rs".into(),
            additions: 1,
            deletions: 0,
            change_type: "added".into(),
            viewer_viewed_state: "unviewed".into(),
        };
        let metadata = GraphqlBlobRepository {
            base: None,
            head: Some(GraphqlBlob {
                typename: "Blob".into(),
                byte_size: Some(12),
                is_binary: Some(false),
                is_truncated: Some(false),
                text: None,
            }),
        };
        let content = GraphqlBlobRepository {
            base: None,
            head: Some(GraphqlBlob {
                typename: "Blob".into(),
                text: Some("fn main() {}".into()),
                ..GraphqlBlob::default()
            }),
        };

        let diff = build_pr_file_diff(file, metadata, content);
        assert_eq!(diff.original.as_deref(), Some(""));
        assert_eq!(diff.modified.as_deref(), Some("fn main() {}"));
        assert!(!diff.binary);
        assert!(!diff.truncated);
        assert!(diff.notice.is_empty());
    }

    #[test]
    fn pr_file_diff_refuses_oversized_text_before_content_loading() {
        let blob = GraphqlBlob {
            typename: "Blob".into(),
            byte_size: Some(MAX_PR_FILE_TEXT_BYTES + 1),
            is_binary: Some(false),
            is_truncated: Some(false),
            text: None,
        };
        assert!(!blob_text_is_loadable(Some(&blob)));
        let diff = build_pr_file_diff(
            PrFile {
                path: "generated.txt".into(),
                additions: 1,
                deletions: 1,
                change_type: "modified".into(),
                viewer_viewed_state: String::new(),
            },
            GraphqlBlobRepository {
                base: Some(blob.clone()),
                head: Some(blob),
            },
            GraphqlBlobRepository {
                base: None,
                head: None,
            },
        );
        assert!(diff.truncated);
        assert!(diff.notice.contains("preview limit"));
        assert!(diff.original.is_none());
        assert!(diff.modified.is_none());
    }
}
