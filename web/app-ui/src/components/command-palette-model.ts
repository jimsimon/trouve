import type { AppRoute, InspectionPanel } from "../router/app-router.js";
import { filterFuzzyTextItems } from "../services/fuzzy-ranking.js";
import type { ProtocolPrInfo } from "../services/protocol-client.js";
import type { SessionVisualState } from "../state/app-store.js";
import {
  sessionIndicatorPresentation,
  type SessionIndicatorFields,
  type SessionIndicatorPresentation,
} from "../state/session-indicator-model.js";
import type { FontAwesomeIconName } from "./font-awesome-icon.js";
import {
  visibleSessionPullRequestBadge,
  type SessionPullRequestBadge,
} from "./session-pull-request-badge.js";

export interface CommandPaletteWorkspace {
  readonly id: string;
  readonly name: string;
}

export interface CommandPaletteSession extends SessionIndicatorFields {
  readonly id: string;
  readonly workspaceId: string;
  readonly title: string;
  readonly branch: string;
  readonly archived: boolean;
  readonly latestThreadId: string | undefined;
  readonly pullRequests: readonly ProtocolPrInfo[];
  readonly state: SessionVisualState;
}

export interface CommandPaletteThread {
  readonly id: string;
  readonly session_id: string;
  readonly mode: string;
  readonly model: string;
  readonly spawned?: boolean;
}

export type CommandPaletteAction =
  | {
      readonly kind: "navigate";
      readonly route: Exclude<AppRoute, { readonly kind: "not-found" }>;
      readonly mobilePane: "thread" | "inspection";
    }
  | {
      readonly kind: "new-session";
      readonly workspaceId: string;
    }
  | {
      readonly kind: "new-thread";
      readonly workspaceId: string;
      readonly sessionId: string;
    };

export type CommandPaletteGroup = "Actions" | "Threads" | "Sessions" | "Views";

export interface CommandPaletteItem {
  readonly id: string;
  readonly group: CommandPaletteGroup;
  readonly label: string;
  readonly detail: string;
  readonly keywords: string;
  readonly icon: FontAwesomeIconName | undefined;
  readonly state?: SessionVisualState;
  readonly sessionIndicator?: SessionIndicatorPresentation;
  readonly pullRequestBadge?: SessionPullRequestBadge;
  readonly current?: boolean;
  readonly action: CommandPaletteAction;
}

export interface CommandPaletteInput {
  readonly route: AppRoute;
  readonly workspaces: readonly CommandPaletteWorkspace[];
  readonly sessions: readonly CommandPaletteSession[];
  /** Threads retained for the active session. Other sessions remain reachable
   * through their session item and latest-thread route. */
  readonly activeThreads: readonly CommandPaletteThread[];
}

const INSPECTION_VIEWS = [
  ["diff", "Diff", "code-compare", "changes patch review"],
  ["files", "Files", "file-lines", "source tree code"],
  ["pr", "Pull request", "code-pull-request", "branch review status"],
  ["mcp", "MCP", "plug", "effective model context protocol tools servers"],
  ["terminal", "Terminal", "terminal", "shell pty console"],
  ["plan", "Plan", "list-check", "todos tasks"],
] as const satisfies readonly [
  InspectionPanel,
  string,
  FontAwesomeIconName,
  string,
][];

const shortModelName = (model: string): string => {
  const segments = model.split("/").filter((segment) => segment !== "");
  return segments.at(-1) ?? model;
};

const workspaceForRoute = (
  route: AppRoute,
  workspaces: readonly CommandPaletteWorkspace[],
): CommandPaletteWorkspace | undefined => {
  if (route.kind === "session") {
    const active = workspaces.find((workspace) => workspace.id === route.workspaceId);
    if (active !== undefined) return active;
  }
  return workspaces[0];
};

/** Build palette entries from the normalized store projection. Commands use
 * the same route and protocol actions as the visible shell controls. */
export const buildCommandPaletteItems = (
  input: CommandPaletteInput,
): readonly CommandPaletteItem[] => {
  const { route, workspaces, sessions, activeThreads } = input;
  const items: CommandPaletteItem[] = [];
  const primaryWorkspace = workspaceForRoute(route, workspaces);

  if (primaryWorkspace !== undefined) {
    items.push({
      id: `action:new-session:${primaryWorkspace.id}`,
      group: "Actions",
      label: "New session",
      detail: primaryWorkspace.name,
      keywords: `create start workspace ${primaryWorkspace.name}`,
      icon: "plus",
      action: { kind: "new-session", workspaceId: primaryWorkspace.id },
    });
  }

  if (route.kind === "session") {
    items.push({
      id: `action:new-thread:${route.sessionId}`,
      group: "Actions",
      label: "New thread",
      detail: sessions.find((session) => session.id === route.sessionId)?.title
        ?? "Current session",
      keywords: "create start conversation tab",
      icon: "plus",
      action: {
        kind: "new-thread",
        workspaceId: route.workspaceId,
        sessionId: route.sessionId,
      },
    });
  }

  for (const workspace of workspaces) {
    if (workspace.id === primaryWorkspace?.id) continue;
    items.push({
      id: `action:new-session:${workspace.id}`,
      group: "Actions",
      label: `New session in ${workspace.name}`,
      detail: workspace.name,
      keywords: `create start workspace ${workspace.name}`,
      icon: "plus",
      action: { kind: "new-session", workspaceId: workspace.id },
    });
  }

  if (route.kind === "session") {
    for (const [index, thread] of activeThreads.entries()) {
      const label = `${thread.mode} · ${shortModelName(thread.model)}`;
      const current = thread.id === route.threadId;
      items.push({
        id: `thread:${thread.id}`,
        group: "Threads",
        label,
        detail: `Thread ${index + 1}`,
        keywords: `${current ? "current " : ""}${thread.id} ${thread.mode} ${thread.model} conversation tab`,
        icon: thread.spawned === true ? "code-branch" : "message",
        current,
        action: {
          kind: "navigate",
          mobilePane: "thread",
          route: {
            kind: "session",
            workspaceId: route.workspaceId,
            sessionId: route.sessionId,
            threadId: thread.id,
            ...(route.inspection === undefined ? {} : { inspection: route.inspection }),
          },
        },
      });
    }
  }

  const workspaceNames = new Map(
    workspaces.map((workspace) => [workspace.id, workspace.name] as const),
  );
  for (const session of sessions) {
    const workspaceName = workspaceNames.get(session.workspaceId) ?? "Workspace";
    const current = route.kind === "session" && route.sessionId === session.id;
    const indicator = sessionIndicatorPresentation(session);
    const pullRequestBadge = visibleSessionPullRequestBadge(
      session.pullRequests,
      session.state,
      current,
    );
    const pullRequestNumbers = [...new Set(session.pullRequests.map(({ number }) => number))]
      .sort((left, right) => right - left);
    const pullRequestDetail = pullRequestNumbers.length === 0
      ? ""
      : ` · ${pullRequestNumbers.length === 1 ? "PR" : "PRs"} ${pullRequestNumbers.map((number) => `#${number}`).join(", ")}`;
    const pullRequestKeywords = pullRequestNumbers
      .map((number) => `${number} #${number} pr ${number}`)
      .join(" ");
    items.push({
      id: `session:${session.id}`,
      group: "Sessions",
      label: session.title,
      detail: `${workspaceName} · ${session.branch}${pullRequestDetail}${session.archived ? " · Archived" : ""}`,
      keywords: `${current ? "current " : ""}${session.id} ${workspaceName} ${session.branch} ${session.state} ${session.attention} ${session.outcome} ${indicator.tooltip} ${pullRequestKeywords} ${session.archived ? "archived" : "active"}`,
      icon: undefined,
      state: session.state,
      sessionIndicator: indicator,
      ...(pullRequestBadge === undefined ? {} : { pullRequestBadge }),
      current,
      action: {
        kind: "navigate",
        mobilePane: "thread",
        route: {
          kind: "session",
          workspaceId: session.workspaceId,
          sessionId: session.id,
          ...(session.latestThreadId === undefined
            ? {}
            : { threadId: session.latestThreadId }),
        },
      },
    });
  }

  items.push(
    {
      id: "view:reviews",
      group: "Views",
      label: "Pull Requests",
      detail: "Review dashboard",
      keywords: "code reviews github branches",
      icon: "code-pull-request",
      action: { kind: "navigate", mobilePane: "thread", route: { kind: "reviews" } },
    },
    {
      id: "view:automations",
      group: "Views",
      label: "Automations",
      detail: "Scheduled prompts",
      keywords: "jobs schedule timer",
      icon: "stopwatch",
      action: { kind: "navigate", mobilePane: "thread", route: { kind: "automations" } },
    },
    {
      id: "view:settings",
      group: "Views",
      label: "Settings",
      detail: "Appearance, providers, models, and integrations",
      keywords: "preferences configuration theme capabilities about",
      icon: "gear",
      action: { kind: "navigate", mobilePane: "thread", route: { kind: "settings" } },
    },
  );

  if (route.kind === "session") {
    for (const [panel, label, icon, keywords] of INSPECTION_VIEWS) {
      items.push({
        id: `view:inspection:${panel}`,
        group: "Views",
        label: `Open ${label}`,
        detail: "Inspection panel",
        keywords,
        icon,
        action: {
          kind: "navigate",
          mobilePane: "inspection",
          route: { ...route, inspection: panel },
        },
      });
    }
  }

  return items;
};

/** Token-aware fuzzy filtering with deterministic source-order tie breaking. */
export const filterCommandPaletteItems = (
  items: readonly CommandPaletteItem[],
  query: string,
): readonly CommandPaletteItem[] => filterFuzzyTextItems(items, query);

export const nextCommandPaletteIndex = (
  key: string,
  currentIndex: number,
  itemCount: number,
): number | undefined => {
  if (itemCount <= 0) return undefined;
  const current = Math.min(itemCount - 1, Math.max(0, currentIndex));
  if (key === "ArrowDown") return (current + 1) % itemCount;
  if (key === "ArrowUp") return (current - 1 + itemCount) % itemCount;
  if (key === "Home") return 0;
  if (key === "End") return itemCount - 1;
  return undefined;
};

export const isCommandPaletteShortcut = (event: {
  readonly key: string;
  readonly ctrlKey: boolean;
  readonly metaKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
  readonly isComposing?: boolean;
  readonly repeat?: boolean;
}): boolean =>
  event.isComposing !== true &&
  event.repeat !== true &&
  !event.altKey &&
  !event.shiftKey &&
  (event.ctrlKey || event.metaKey) &&
  event.key.toLowerCase() === "k";
