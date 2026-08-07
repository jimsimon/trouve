import type {
  ProtocolGithubPrList,
  ProtocolPrInfo,
} from "../services/protocol-client.js";
import {
  projectSessionPullRequests,
  type SessionPullRequestIdentity,
  type SessionVisualState,
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

/** Account snapshots are also the established sidebar source. Match a
 * session by the server-enriched workspace id and exact head branch, then keep
 * open PRs first and newest PRs first within each terminal-state group. */
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

/** Apply the same precedence used by every session navigator:
 * attention, failure, unread, and busy indicators win over pull-request state.
 * A selected completed session has already cleared its local unread marker, so
 * it can hand off to the pull-request badge during the intervening render. */
export const visibleSessionPullRequestBadge = (
  prs: readonly ProtocolPrInfo[],
  state: SessionVisualState,
  selected: boolean,
): SessionPullRequestBadge | undefined => {
  const badge = sessionPullRequestBadge(prs);
  if (badge === undefined) return undefined;
  return state === "idle" || (state === "done" && selected)
    ? badge
    : undefined;
};
