import type {
  ProtocolGithubPrList,
  ProtocolPrInfo,
} from "../services/protocol-client.js";
import type { SessionListItem } from "../state/app-store.js";
import type { FontAwesomeIconName } from "./font-awesome-icon.js";

export type PullRequestGroupKey =
  | "review-requested"
  | "drafts"
  | "needs-reviewers"
  | "pending-review"
  | "ready-to-merge"
  | "needs-attention"
  | "recently-merged";

export type PullRequestPillTone = "neutral" | "ok" | "warning" | "danger";

export interface PullRequestPill {
  readonly label: string;
  readonly tone: PullRequestPillTone;
}

export interface PullRequestReviewFinding {
  readonly location: string;
  readonly title: string;
  readonly severity: string;
  readonly confidence: string;
  readonly body: string;
  readonly prompt: string;
  readonly status: string;
  readonly publicationStatus: string;
  readonly origin: string;
  readonly rootCause: string;
  readonly recommendation: string;
  readonly executionPath: string;
  readonly consequence: string;
  readonly regressionTest: string;
}

export interface PullRequestRow {
  readonly key: string;
  readonly workspaceId: string;
  readonly repository: string;
  readonly number: number;
  readonly title: string;
  readonly branch: string;
  readonly check: PullRequestPill;
  readonly approval: PullRequestPill;
  readonly merge: PullRequestPill | undefined;
  readonly commentsLabel: string;
  readonly lastComment: string;
  readonly url: string;
  readonly hasChat: boolean;
  readonly reviewSummary: string;
  readonly reviewPrompt: string;
  readonly reviewFindings: readonly PullRequestReviewFinding[];
}

export interface PullRequestGroupDefinition {
  readonly key: PullRequestGroupKey;
  readonly title: string;
  readonly description: string;
  readonly icon: FontAwesomeIconName;
  readonly tone: "accent" | "muted" | "warning" | "ok" | "danger" | "tint";
  readonly emptyText: string;
}

export interface PullRequestGroup extends PullRequestGroupDefinition {
  readonly position: number;
  readonly groupCount: number;
  readonly collapsed: boolean;
  readonly pullRequests: readonly PullRequestRow[];
}

export const PULL_REQUEST_GROUPS = Object.freeze([
  {
    key: "review-requested",
    title: "Review Requested",
    description: "Pull requests where your review has been requested.",
    icon: "circle-dot",
    tone: "accent",
    emptyText: "No reviews waiting on you.",
  },
  {
    key: "drafts",
    title: "Drafts",
    description: "Open pull requests still marked as drafts.",
    icon: "pen",
    tone: "muted",
    emptyText: "No draft pull requests right now.",
  },
  {
    key: "needs-reviewers",
    title: "Needs Reviewers",
    description: "Open pull requests that do not have any reviewers yet.",
    icon: "user-plus",
    tone: "warning",
    emptyText: "Every open pull request has a reviewer.",
  },
  {
    key: "pending-review",
    title: "Pending Review",
    description: "Open pull requests waiting for review or approval.",
    icon: "circle-half-stroke",
    tone: "warning",
    emptyText: "Nothing is waiting on review.",
  },
  {
    key: "ready-to-merge",
    title: "Ready to Merge",
    description: "Fully approved pull requests with every check passing.",
    icon: "check",
    tone: "ok",
    emptyText: "Nothing is ready to merge yet.",
  },
  {
    key: "needs-attention",
    title: "Needs Attention",
    description: "Pull requests with merge conflicts, failing checks, or changes requested.",
    icon: "triangle-exclamation",
    tone: "danger",
    emptyText: "Nothing needs attention — all clear.",
  },
  {
    key: "recently-merged",
    title: "Recently Merged",
    description: "Pull requests merged in the last 24 hours.",
    icon: "code-merge",
    tone: "tint",
    emptyText: "Nothing merged in the last 24 hours.",
  },
] as const satisfies readonly PullRequestGroupDefinition[]);

export const PULL_REQUEST_GROUP_KEYS = Object.freeze(
  PULL_REQUEST_GROUPS.map(({ key }) => key),
);

const definitionByKey = new Map(
  PULL_REQUEST_GROUPS.map((definition) => [definition.key, definition] as const),
);

const requestedReviewers = (pr: ProtocolPrInfo): readonly string[] =>
  pr.requested_reviewers ?? [];

const normalizedReviewState = (state: string):
  | "approved"
  | "changes_requested"
  | "dismissed"
  | "other" => {
  switch (state.toLowerCase()) {
    case "approved": return "approved";
    case "changesrequested":
    case "changes_requested": return "changes_requested";
    case "dismissed": return "dismissed";
    default: return "other";
  }
};

const reviewVerdicts = (pr: ProtocolPrInfo): ReadonlyMap<string, boolean> => {
  const verdicts = new Map<string, boolean>();
  for (const review of pr.reviews) {
    switch (normalizedReviewState(review.state)) {
      case "approved": verdicts.set(review.reviewer, true); break;
      case "changes_requested": verdicts.set(review.reviewer, false); break;
      case "dismissed": verdicts.delete(review.reviewer); break;
      case "other": break;
    }
  }
  return verdicts;
};

const failingConclusions = new Set([
  "failure",
  "timed_out",
  "cancelled",
  "action_required",
  "startup_failure",
  "stale",
]);

export const pullRequestCheckPill = (pr: ProtocolPrInfo): PullRequestPill => {
  if (pr.checks.length === 0) return { label: "no checks", tone: "neutral" };
  if (pr.checks.some((check) =>
    check.conclusion !== null &&
    check.conclusion !== undefined &&
    failingConclusions.has(check.conclusion))) {
    return { label: "checks failing", tone: "danger" };
  }
  if (pr.checks.some((check) =>
    check.status !== "completed" || check.conclusion === null || check.conclusion === undefined)) {
    return { label: "checks running", tone: "warning" };
  }
  return { label: "checks passing", tone: "ok" };
};

export const pullRequestApprovalPill = (pr: ProtocolPrInfo): PullRequestPill => {
  const verdicts = reviewVerdicts(pr);
  const approvals = [...verdicts.values()].filter(Boolean).length;
  if ([...verdicts.values()].some((approved) => !approved)) {
    return { label: "changes requested", tone: "danger" };
  }
  if (approvals > 0 && requestedReviewers(pr).length === 0) {
    return { label: "approved", tone: "ok" };
  }
  if (approvals > 0 || requestedReviewers(pr).length > 0) {
    return { label: "review pending", tone: "warning" };
  }
  return { label: "no reviews", tone: "neutral" };
};

export const pullRequestMergePill = (
  pr: ProtocolPrInfo,
): PullRequestPill | undefined => {
  if (pr.state !== "open") return undefined;
  if (pr.mergeable === true) return { label: "no conflicts", tone: "ok" };
  if (pr.mergeable === false) return { label: "merge conflicts", tone: "danger" };
  return { label: "merge unknown", tone: "neutral" };
};

const validDateMilliseconds = (value: string | null | undefined): number | undefined => {
  if (value === null || value === undefined || value === "") return undefined;
  const milliseconds = Date.parse(value);
  return Number.isFinite(milliseconds) ? milliseconds : undefined;
};

/** Client-side counterpart of the established dashboard
 * classifier. Priority is significant: a non-draft merge conflict wins over
 * the viewer's review request, while a conflicted draft remains a draft. */
export const classifyPullRequest = (
  pr: ProtocolPrInfo,
  viewer: string,
  now: Date,
): PullRequestGroupKey | undefined => {
  if (pr.state === "merged") {
    const mergedAt = validDateMilliseconds(pr.merged_at);
    return mergedAt !== undefined && now.getTime() - mergedAt <= 24 * 60 * 60 * 1_000
      ? "recently-merged"
      : undefined;
  }
  if (pr.state !== "open") return undefined;
  if (pr.mergeable === false && !pr.draft) return "needs-attention";
  if (viewer !== "" && requestedReviewers(pr).includes(viewer)) {
    return "review-requested";
  }
  if (pr.draft) return "drafts";
  const checks = pullRequestCheckPill(pr);
  const approval = pullRequestApprovalPill(pr);
  if (checks.tone === "danger" || approval.tone === "danger") {
    return "needs-attention";
  }
  if (approval.tone === "ok" && checks.tone === "ok") return "ready-to-merge";
  if (requestedReviewers(pr).length === 0 && pr.reviews.length === 0) {
    return "needs-reviewers";
  }
  return "pending-review";
};

export const pullRequestHumanAge = (value: string, now: Date): string => {
  const milliseconds = validDateMilliseconds(value);
  if (milliseconds === undefined) return "time unavailable";
  const minutes = Math.max(0, Math.floor((now.getTime() - milliseconds) / 60_000));
  if (minutes === 0) return "just now";
  if (minutes === 1) return "1 min ago";
  if (minutes < 60) return `${minutes} mins ago`;
  if (minutes < 120) return "1 hour ago";
  if (minutes < 60 * 24) return `${Math.floor(minutes / 60)} hours ago`;
  if (minutes < 60 * 48) return "1 day ago";
  return `${Math.floor(minutes / (60 * 24))} days ago`;
};

const canonicalOrder = (): readonly PullRequestGroupKey[] =>
  PULL_REQUEST_GROUP_KEYS;

export const reconcilePullRequestGroupOrder = (
  saved: readonly string[],
): { readonly order: readonly PullRequestGroupKey[]; readonly changed: boolean } => {
  const seen = new Set<PullRequestGroupKey>();
  const order: PullRequestGroupKey[] = [];
  for (const key of saved) {
    if (!definitionByKey.has(key as PullRequestGroupKey)) continue;
    const known = key as PullRequestGroupKey;
    if (seen.has(known)) continue;
    seen.add(known);
    order.push(known);
  }
  for (const key of canonicalOrder()) {
    if (seen.has(key)) continue;
    seen.add(key);
    order.push(key);
  }
  const changed = order.length !== saved.length ||
    order.some((key, index) => key !== saved[index]);
  return { order: Object.freeze(order), changed };
};

export const movePullRequestGroup = (
  current: readonly string[],
  key: PullRequestGroupKey,
  offset: number,
): readonly PullRequestGroupKey[] => {
  const order = [...reconcilePullRequestGroupOrder(current).order];
  const source = order.indexOf(key);
  if (source === -1 || order.length === 0) return Object.freeze(order);
  const target = Math.max(0, Math.min(order.length - 1, source + Math.trunc(offset)));
  if (target === source) return Object.freeze(order);
  order.splice(source, 1);
  order.splice(target, 0, key);
  return Object.freeze(order);
};

export const reorderPullRequestGroup = (
  current: readonly string[],
  key: PullRequestGroupKey,
  targetKey: PullRequestGroupKey,
  after: boolean,
): readonly PullRequestGroupKey[] => {
  const order = [...reconcilePullRequestGroupOrder(current).order];
  if (key === targetKey) return Object.freeze(order);
  const source = order.indexOf(key);
  if (source === -1 || !order.includes(targetKey)) return Object.freeze(order);
  order.splice(source, 1);
  const target = order.indexOf(targetKey);
  order.splice(target + (after ? 1 : 0), 0, key);
  return Object.freeze(order);
};

const repositoryKey = (list: ProtocolGithubPrList, pr: ProtocolPrInfo): string => {
  const host = pr.host?.trim() || list.host;
  const repository = pr.repository?.trim() || "repository";
  return `${host}/${repository}`;
};

export const pullRequestRepositories = (
  lists: readonly ProtocolGithubPrList[],
): readonly string[] => Object.freeze([
  ...new Set(lists.flatMap((list) => list.prs.map((pr) => repositoryKey(list, pr)))),
].sort((left, right) => left.localeCompare(right)));

const rowFromPullRequest = (
  list: ProtocolGithubPrList,
  pr: ProtocolPrInfo,
  sessions: readonly SessionListItem[],
  now: Date,
): PullRequestRow => {
  const repository = pr.repository?.trim() || "Repository";
  const workspaceId = pr.workspace_id ?? "";
  const review = pr.trouve_review ?? undefined;
  const themes = new Map((review?.themes ?? []).map((theme) => [theme.id, theme]));
  const findings = (review?.findings ?? [])
    .filter((finding) => finding.status === "open")
    .map((finding) => {
      const theme = (finding.theme_ids ?? []).map((id) => themes.get(id)).find(Boolean);
      return Object.freeze({
      location: `${finding.path}:${finding.line}`,
      title: finding.title,
      severity: finding.severity,
      confidence: finding.confidence ?? "medium",
      body: finding.body,
      prompt: finding.prompt_for_agents ?? "",
      status: finding.status,
      publicationStatus: finding.github_publication_status ?? "pending",
      origin: finding.origin ?? "new_change",
      rootCause: theme?.root_cause ?? "",
      recommendation: theme?.recommendation ?? "",
      executionPath: finding.evidence?.execution_path ?? "",
      consequence: finding.evidence?.consequence ?? "",
      regressionTest: finding.evidence?.regression_test ?? "",
    });
    });
  const comments = pr.comments ?? 0;
  const lastComment = pr.last_comment_at === null || pr.last_comment_at === undefined
    ? comments === 0 ? "no comments yet" : "last comment time unavailable"
    : `last comment ${pullRequestHumanAge(pr.last_comment_at, now)}`;
  return Object.freeze({
    key: `${repositoryKey(list, pr)}#${pr.number}`,
    workspaceId,
    repository,
    number: pr.number,
    title: pr.title,
    branch: pr.head,
    check: pullRequestCheckPill(pr),
    approval: pullRequestApprovalPill(pr),
    merge: pullRequestMergePill(pr),
    commentsLabel: `${comments} comment${comments === 1 ? "" : "s"}`,
    lastComment,
    url: pr.url,
    hasChat: workspaceId !== "" && sessions.some((session) =>
      session.workspaceId === workspaceId && session.branch === pr.head),
    reviewSummary: review?.summary ?? "",
    reviewPrompt: review?.prompt_for_agents ?? "",
    reviewFindings: Object.freeze(findings),
  });
};

export const buildPullRequestGroups = (
  lists: readonly ProtocolGithubPrList[],
  sessions: readonly SessionListItem[],
  options: {
    readonly order?: readonly string[];
    readonly collapsed?: ReadonlySet<string>;
    readonly repository?: string;
    readonly now?: Date;
  } = {},
): readonly PullRequestGroup[] => {
  const now = options.now ?? new Date();
  const rows = new Map<PullRequestGroupKey, { row: PullRequestRow; sort: number }[]>();
  for (const list of lists) {
    for (const pr of list.prs) {
      if (options.repository !== undefined && repositoryKey(list, pr) !== options.repository) {
        continue;
      }
      const key = classifyPullRequest(pr, list.viewer ?? "", now);
      if (key === undefined) continue;
      const mergedAt = validDateMilliseconds(pr.merged_at);
      const sort = key === "recently-merged" && mergedAt !== undefined
        ? mergedAt
        : pr.number;
      const groupRows = rows.get(key) ?? [];
      groupRows.push({ row: rowFromPullRequest(list, pr, sessions, now), sort });
      rows.set(key, groupRows);
    }
  }
  const order = reconcilePullRequestGroupOrder(options.order ?? []).order;
  return Object.freeze(order.map((key, position) => {
    const definition = definitionByKey.get(key)!;
    const pullRequests = (rows.get(key) ?? [])
      .sort((left, right) =>
        right.sort - left.sort || left.row.key.localeCompare(right.row.key))
      .map(({ row }) => row);
    return Object.freeze({
      ...definition,
      position,
      groupCount: order.length,
      collapsed: options.collapsed?.has(key) ?? false,
      pullRequests: Object.freeze(pullRequests),
    });
  }));
};
