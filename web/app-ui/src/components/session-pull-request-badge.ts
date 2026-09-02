import type {
  ProtocolGithubPrList,
  ProtocolPrInfo,
} from "../services/protocol-client.js";
import {
  projectSessionPullRequests,
  type SessionPullRequestIdentity,
} from "../state/app-store.js";

export type { SessionPullRequestIdentity } from "../state/app-store.js";

export type SessionPullRequestBadgeTone =
  | "ready"
  | "blocked"
  | "merged"
  | "closed";

export interface SessionPullRequestBadge {
  readonly tone: SessionPullRequestBadgeTone;
  readonly label: string;
  readonly tooltip: string;
  readonly count: number;
}

interface PullRequestBadgeStatus {
  readonly tone: SessionPullRequestBadgeTone;
  readonly label: string;
}

const pullRequestStatus = (pr: ProtocolPrInfo): PullRequestBadgeStatus => {
  if (pr.state === "merged") return { tone: "merged", label: "Merged" };
  if (pr.state === "closed") return { tone: "closed", label: "Closed" };
  const mergeState = pr.merge_state_status?.toLowerCase();
  if (
    pr.state === "open" &&
    !pr.draft &&
    (mergeState === "clean" || mergeState === "has_hooks")
  ) {
    return { tone: "ready", label: "Ready to merge" };
  }
  return {
    tone: "blocked",
    label: pr.state === "open" && pr.draft
      ? "Unable to merge · Draft"
      : "Unable to merge",
  };
};

/** Account snapshots are also the established sidebar source. Matching
 * session-branch PRs are presented open-first and newest-first. */
export const pullRequestsForSession = (
  session: SessionPullRequestIdentity,
  lists: readonly ProtocolGithubPrList[],
): readonly ProtocolPrInfo[] => projectSessionPullRequests(session, lists);

/** Navigation badge aggregation: every
 * open PR must be GitHub-ready before the aggregate becomes green. */
export const sessionPullRequestBadge = (
  prs: readonly ProtocolPrInfo[],
): SessionPullRequestBadge | undefined => {
  if (prs.length === 0) return undefined;
  const open = prs.filter((pr) => pr.state === "open");
  const status = open.length > 0
    ? open.every((pr) => pullRequestStatus(pr).tone === "ready")
      ? { tone: "ready", label: "Ready to merge" } as const
      : { tone: "blocked", label: "Unable to merge" } as const
    : pullRequestStatus(prs[0]!);
  const heading = prs.length === 1 ? "Pull request" : `${prs.length} pull requests`;
  const lines = prs.map((pr) => `#${pr.number} · ${pullRequestStatus(pr).label}`);
  return Object.freeze({
    ...status,
    count: prs.length,
    tooltip: `${heading}\n${lines.join("\n")}`,
  });
};

/** Keep pull-request state independent from the session's work status so
 * navigators can present both indicators at the same time. */
export const visibleSessionPullRequestBadge = (
  prs: readonly ProtocolPrInfo[],
): SessionPullRequestBadge | undefined => sessionPullRequestBadge(prs);
