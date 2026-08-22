import type { InboxSessionOrderFields } from "../state/session-inbox-model.js";

export type WorkspaceSessionStatus =
  | "attention"
  | "unread"
  | "working"
  | "draft"
  | "done";
export type WorkspaceSessionPullRequest = "draft" | "open" | "merged" | "closed" | "none";
export type WorkspaceSessionGrouping = "repository" | "workspace" | "updated" | "status";
export type WorkspaceSessionOrdering = "updated" | "status" | "created";

export const WORKSPACE_STATUS_FILTERS = [
  ["attention", "Needs attention"],
  ["unread", "Unread"],
  ["working", "Working"],
  ["draft", "Draft"],
  ["done", "Done"],
] as const;

export const WORKSPACE_PULL_REQUEST_FILTERS = [
  ["draft", "PR draft"],
  ["open", "PR open"],
  ["merged", "PR merged"],
  ["closed", "PR closed"],
  ["none", "No PR"],
] as const;

export interface WorkspaceSessionListFields extends InboxSessionOrderFields {
  readonly createdAt: string;
  readonly pullRequestKind: WorkspaceSessionPullRequest;
}

export interface WorkspaceSessionSection<T> {
  readonly key: string;
  readonly label: string;
  readonly sessions: readonly T[];
}

export interface WorkspaceSessionOrganization<T> {
  readonly sections: readonly WorkspaceSessionSection<T>[];
  readonly archived: readonly T[];
}

export const pullRequestKind = (
  pullRequests: readonly { readonly state: string; readonly draft?: boolean }[],
): WorkspaceSessionPullRequest => {
  if (pullRequests.some((pullRequest) => pullRequest.state === "open" && pullRequest.draft)) {
    return "draft";
  }
  if (pullRequests.some((pullRequest) => pullRequest.state === "open")) return "open";
  if (pullRequests.some((pullRequest) => pullRequest.state === "merged")) return "merged";
  if (pullRequests.some((pullRequest) => pullRequest.state === "closed")) return "closed";
  return "none";
};

export const workspaceSessionStatus = (
  session: WorkspaceSessionListFields,
): WorkspaceSessionStatus => {
  if (session.attention !== "none" || (session.outcome === "failed" && session.unread !== false)) {
    return "attention";
  }
  if (session.unread === true) return "unread";
  if (session.active || session.outcome === "running") return "working";
  if (
    session.archived
    || session.pullRequestKind === "merged"
    || session.pullRequestKind === "closed"
  ) return "done";
  return "draft";
};

const statusIndex = (session: WorkspaceSessionListFields): number =>
  WORKSPACE_STATUS_FILTERS.findIndex(([status]) => status === workspaceSessionStatus(session));
const pullRequestIndex = (session: WorkspaceSessionListFields): number =>
  WORKSPACE_PULL_REQUEST_FILTERS.findIndex(([kind]) => kind === session.pullRequestKind);
const epoch = (value: string): number => {
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : Number.NEGATIVE_INFINITY;
};

const compare = (
  left: WorkspaceSessionListFields,
  right: WorkspaceSessionListFields,
  ordering: WorkspaceSessionOrdering,
): number => {
  const newestUpdated = (): number =>
    epoch(right.updatedAt) - epoch(left.updatedAt)
    || epoch(right.createdAt) - epoch(left.createdAt);
  if (ordering === "status") {
    return statusIndex(left) - statusIndex(right) || newestUpdated() || left.id.localeCompare(right.id);
  }
  if (ordering === "created") {
    return epoch(right.createdAt) - epoch(left.createdAt) || left.id.localeCompare(right.id);
  }
  return newestUpdated() || left.id.localeCompare(right.id);
};

const calendarDayOrdinal = (value: Date, timeZone?: string): number => {
  if (timeZone === undefined) {
    return Date.UTC(value.getFullYear(), value.getMonth(), value.getDate()) / 86_400_000;
  }
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone,
    year: "numeric",
    month: "numeric",
    day: "numeric",
  }).formatToParts(value);
  const part = (type: Intl.DateTimeFormatPartTypes): number =>
    Number(parts.find((candidate) => candidate.type === type)?.value);
  return Date.UTC(part("year"), part("month") - 1, part("day")) / 86_400_000;
};

export const workspaceSessionUpdatedGroup = (
  updatedAt: string,
  now: number,
  timeZone?: string,
): readonly [string, string] => {
  const updated = new Date(updatedAt);
  const current = new Date(now);
  if (!Number.isFinite(updated.getTime()) || !Number.isFinite(current.getTime())) {
    return ["older", "Older"];
  }
  const age = calendarDayOrdinal(current, timeZone) - calendarDayOrdinal(updated, timeZone);
  if (age <= 0) return ["today", "Today"];
  if (age === 1) return ["yesterday", "Yesterday"];
  if (age <= 7) return ["previous-7-days", "Previous 7 days"];
  return ["older", "Older"];
};

export const workspaceSessionSectionCollapsed = <T extends { readonly id: string }>(
  section: WorkspaceSessionSection<T>,
  storedCollapsed: boolean,
  selectedSessionId: string | undefined,
): boolean =>
  section.label !== ""
  && storedCollapsed
  && !section.sessions.some(({ id }) => id === selectedSessionId);

const statusLabel = (status: WorkspaceSessionStatus): string =>
  WORKSPACE_STATUS_FILTERS.find(([candidate]) => candidate === status)?.[1] ?? "Draft";

export const organizeWorkspaceSessions = <T extends WorkspaceSessionListFields>(
  sessions: readonly T[],
  options: {
    readonly workspaceId: string;
    readonly grouping: WorkspaceSessionGrouping;
    readonly ordering: WorkspaceSessionOrdering;
    readonly statusFilter: number;
    readonly pullRequestFilter: number;
    readonly now: number;
    readonly timeZone?: string;
  },
): WorkspaceSessionOrganization<T> => {
  const visible = sessions.filter((session) =>
    (options.workspaceId === "" || session.workspaceId === options.workspaceId)
    && (options.statusFilter & (1 << statusIndex(session))) !== 0
    && (options.pullRequestFilter & (1 << pullRequestIndex(session))) !== 0);
  const sorted = (values: readonly T[]): readonly T[] =>
    [...values].sort((left, right) => compare(left, right, options.ordering));
  const active = sorted(visible.filter((session) => !session.archived));
  const archived = sorted(visible.filter((session) => session.archived));
  if (options.grouping !== "updated" && options.grouping !== "status") {
    return Object.freeze({
      sections: active.length === 0
        ? Object.freeze([])
        : Object.freeze([Object.freeze({ key: "all", label: "", sessions: active })]),
      archived,
    });
  }
  const groups = new Map<string, { label: string; sessions: T[] }>();
  for (const session of active) {
    const [key, label] = options.grouping === "updated"
      ? workspaceSessionUpdatedGroup(session.updatedAt, options.now, options.timeZone)
      : [workspaceSessionStatus(session), statusLabel(workspaceSessionStatus(session))];
    const group = groups.get(key) ?? { label, sessions: [] };
    group.sessions.push(session);
    groups.set(key, group);
  }
  const keys = options.grouping === "updated"
    ? ["today", "yesterday", "previous-7-days", "older"]
    : WORKSPACE_STATUS_FILTERS.map(([status]) => status);
  return Object.freeze({
    sections: Object.freeze(keys.flatMap((key) => {
      const group = groups.get(key);
      return group === undefined
        ? []
        : [Object.freeze({ key, label: group.label, sessions: Object.freeze(group.sessions) })];
    })),
    archived,
  });
};
