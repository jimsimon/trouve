import type {
  ProtocolCreatePrRequest,
  ProtocolGithubIntegration,
  ProtocolPrInfo,
} from "../services/protocol-client.js";

export type PrSummaryTone = "ready" | "pending" | "warning" | "failed" | "muted";

export interface PrSummary {
  readonly label: string;
  readonly tone: PrSummaryTone;
}

export interface CreatePrDraft {
  readonly title: string;
  readonly body: string;
  readonly base: string;
  readonly draft: boolean;
}

export const githubIntegrationConfigured = (
  integration: ProtocolGithubIntegration,
): boolean => {
  const hosts = integration.hosts;
  return hosts === undefined || hosts.length === 0
    ? integration.configured
    : hosts.some((host) => host.configured);
};

export const safeSessionPrHref = (
  value: string | null | undefined,
): string | undefined => {
  if (value === undefined || value === null || value.trim() === "") return undefined;
  try {
    const url = new URL(value);
    if (
      url.protocol !== "https:" ||
      url.host === "" ||
      url.username !== "" ||
      url.password !== ""
    ) return undefined;
    return url.href;
  } catch {
    return undefined;
  }
};

const successfulConclusion = (value: string): boolean =>
  value === "success" || value === "neutral" || value === "skipped";

export const checkSummary = (pr: ProtocolPrInfo): PrSummary => {
  if (pr.checks.length === 0) return { label: "No checks reported", tone: "muted" };
  const pending = pr.checks.filter(
    (check) => check.status !== "completed" || check.conclusion === null || check.conclusion === undefined,
  ).length;
  const failed = pr.checks.filter(
    (check) => check.status === "completed" &&
      check.conclusion !== null &&
      check.conclusion !== undefined &&
      !successfulConclusion(check.conclusion),
  ).length;
  if (failed > 0) {
    return {
      label: `${failed} failing · ${pr.checks.length - failed - pending} passing${pending > 0 ? ` · ${pending} pending` : ""}`,
      tone: "failed",
    };
  }
  if (pending > 0) {
    return {
      label: `${pr.checks.length - pending} passing · ${pending} pending`,
      tone: "pending",
    };
  }
  return { label: `${pr.checks.length} passing`, tone: "ready" };
};

export const reviewSummary = (pr: ProtocolPrInfo): PrSummary => {
  const approvals = pr.reviews.filter((review) => review.state.toLowerCase() === "approved").length;
  const changes = pr.reviews.filter(
    (review) => review.state.toLowerCase() === "changes_requested",
  ).length;
  const requested = pr.requested_reviewers?.length ?? 0;
  if (changes > 0) {
    return {
      label: `${changes} requesting changes${approvals > 0 ? ` · ${approvals} approved` : ""}`,
      tone: "failed",
    };
  }
  if (requested > 0) {
    return {
      label: `${approvals} approved · ${requested} awaiting review`,
      tone: "pending",
    };
  }
  if (approvals > 0) return { label: `${approvals} approved`, tone: "ready" };
  return pr.reviews.length === 0
    ? { label: "No reviews", tone: "muted" }
    : { label: `${pr.reviews.length} review${pr.reviews.length === 1 ? "" : "s"}`, tone: "muted" };
};

export const mergeabilitySummary = (pr: ProtocolPrInfo): PrSummary => {
  if (pr.state !== "open") {
    return {
      label: pr.state === "merged" ? "Merged" : "Closed",
      tone: pr.state === "merged" ? "ready" : "muted",
    };
  }
  if (pr.draft) return { label: "Draft", tone: "pending" };
  if (pr.mergeable === false) return { label: "Merge conflicts", tone: "failed" };
  switch (pr.merge_state_status?.toLowerCase()) {
    case "clean": return { label: "Ready to merge", tone: "ready" };
    case "blocked": return { label: "Merge blocked", tone: "failed" };
    case "behind": return { label: "Behind base branch", tone: "warning" };
    case "dirty": return { label: "Merge conflicts", tone: "failed" };
    case "unstable": return { label: "Checks are unstable", tone: "warning" };
    case "draft": return { label: "Draft", tone: "pending" };
    default:
      return pr.mergeable === true
        ? { label: "Mergeable", tone: "ready" }
        : { label: "Mergeability pending", tone: "pending" };
  }
};

export const canMergePr = (pr: ProtocolPrInfo): boolean =>
  pr.state === "open" && !pr.draft && pr.mergeable !== false;

export const createPrRequest = (draft: CreatePrDraft): ProtocolCreatePrRequest => {
  const title = draft.title.trim();
  if (title === "") throw new Error("title required");
  const base = draft.base.trim();
  return {
    title,
    body: draft.body.trim(),
    ...(base === "" ? {} : { base }),
    draft: draft.draft,
  };
};

export type MergeMethod = "merge" | "squash" | "rebase";

export const mergeMethod = (value: string): MergeMethod =>
  value === "squash" || value === "rebase" ? value : "merge";
