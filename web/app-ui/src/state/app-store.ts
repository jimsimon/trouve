import type {
  ProtocolEventEnvelope,
  ProtocolGithubPrList,
  ProtocolGitWorktreeSettings,
  ProtocolPrInfo,
  ProtocolSession,
  ProtocolServerProjection,
  ProtocolServerInfo,
  ProtocolSessionSummary,
  ProtocolThread,
  ProtocolThreadStatus,
  ProtocolThreadViewSnapshot,
  ProtocolThreadToolDetails,
  ProtocolTodoItem,
  ProtocolWorkspace,
} from "../services/protocol-client.js";
import {
  createComputed,
  createSignal,
  type ReadonlySignal,
} from "./reactivity.js";
import { sortInboxSessions } from "./session-inbox-model.js";
import { ThreadViewModel } from "./thread-view-model.js";
import type { QueuedPrompt } from "./thread-view-model.js";

const MAX_CACHED_THREAD_HISTORY_ITEMS = 2_048;

export type SessionVisualState =
  | "running"
  | "attention"
  | "idle"
  | "done"
  | "failed";

/** Render-facing selector result. Wire objects stay normalized in the store;
 * components consume this stable domain shape rather than protocol casing. */
export interface SessionListItem {
  readonly id: string;
  readonly workspaceId: string;
  readonly title: string;
  readonly branch: string;
  readonly archived: boolean;
  readonly active: boolean;
  readonly attention: ProtocolSessionSummary["attention"];
  readonly outcome: ProtocolSessionSummary["outcome"];
  readonly latestThreadId: string | undefined;
  readonly updatedAt: string;
  readonly state: SessionVisualState;
  /** Client-local seen/latest comparison. Durable outcomes describe what
   * happened; this flag describes whether this frontend has presented it. */
  readonly unread: boolean;
}

export interface ThreadIndicatorState {
  readonly active: boolean;
  readonly attention: ProtocolThreadStatus["attention"];
  readonly outcome: ProtocolThreadStatus["outcome"];
  readonly unread: boolean;
}

export interface SessionPullRequestIdentity {
  readonly workspaceId: string;
  readonly branch: string;
}

interface SessionPullRequestMentions {
  readonly urls: ReadonlySet<string>;
  readonly numbers: ReadonlySet<number>;
}

/** Latest durable account-level PR slice for one GitHub host. The event
 * timestamp drives freshness UI; the per-host cursor prevents a delayed
 * replay from replacing newer account data. */
export interface GithubPullRequestSnapshot {
  readonly cursor: number;
  readonly refreshedAt: string;
  readonly pullRequests: ProtocolGithubPrList;
}

export interface GitWorktreeSettingsSnapshot {
  readonly cursor: number;
  readonly settings: ProtocolGitWorktreeSettings;
}

const visualState = (
  summary: ProtocolSessionSummary,
  unread: boolean,
): SessionVisualState => {
  if (summary.attention !== "none") return "attention";
  if (summary.active || summary.outcome === "running") return "running";
  if (unread && summary.outcome === "failed") return "failed";
  if (unread && summary.outcome === "succeeded") return "done";
  return "idle";
};

interface TerminalStatusFields {
  readonly active: boolean;
  readonly outcome: ProtocolSessionSummary["outcome"];
}

const terminalSummary = (summary: TerminalStatusFields): boolean =>
  !summary.active
  && (summary.outcome === "succeeded" || summary.outcome === "failed");

/** A live full-replacement event represents a newly completed turn only when
 * it crosses into a terminal state (or changes terminal outcome). Other
 * summary updates may advance latest_cursor for metadata/attention changes and
 * must not resurrect a terminal badge the user already saw. */
const liveTerminalTransition = (
  previous: TerminalStatusFields | undefined,
  next: TerminalStatusFields,
): boolean => terminalSummary(next) && (
  previous === undefined
  || previous.active
  || previous.outcome === "running"
  || previous.outcome !== next.outcome
);

/** Compatibility projection for protocol servers that predate the atomic
 * session-summary endpoint. Session metadata is fetched after a server-cursor
 * fence, so these fields are at least as fresh as that replay boundary. */
export const fallbackSessionSummary = (
  session: ProtocolSession,
): ProtocolSessionSummary => ({
  session_id: session.id,
  workspace_id: session.workspace_id,
  archived: session.archived ?? false,
  active: session.active ?? false,
  attention: "none",
  outcome: session.active === true ? "running" : "idle",
  latest_cursor: 0,
  updated_at: session.created_at,
});

const toListItem = (
  summary: ProtocolSessionSummary,
  metadata: ProtocolSession | undefined,
  seenCursor: number,
): SessionListItem => {
  const unread = !summary.active
    && (summary.outcome === "succeeded" || summary.outcome === "failed")
    && summary.latest_cursor > seenCursor;
  return {
    id: summary.session_id,
    workspaceId: summary.workspace_id,
    title: metadata?.title ?? "Untitled session",
    branch: metadata?.branch ?? summary.session_id,
    archived: summary.archived,
    active: summary.active,
    attention: summary.attention,
    outcome: summary.outcome,
    ...(summary.latest_thread_id == null
      ? { latestThreadId: undefined }
      : { latestThreadId: summary.latest_thread_id }),
    updatedAt: summary.updated_at,
    state: visualState(summary, unread),
    unread,
  };
};

const samePullRequest = (
  left: ProtocolPrInfo,
  right: ProtocolPrInfo,
): boolean =>
  left.number === right.number &&
  (left.host ?? "").toLowerCase() === (right.host ?? "").toLowerCase() &&
  (left.repository ?? "").toLowerCase() === (right.repository ?? "").toLowerCase();

/** Web counterpart of the native shared PR projection. Exact account-feed
 * branch matches are intrinsically associated with the session. The
 * authoritative session endpoint may add cross-branch PRs discovered from
 * durable session activity; subsequent account snapshots update those known
 * PRs without broadening the association. */
export const projectSessionPullRequests = (
  session: SessionPullRequestIdentity,
  lists: readonly ProtocolGithubPrList[],
  known: readonly ProtocolPrInfo[] = [],
  mentions?: SessionPullRequestMentions,
): readonly ProtocolPrInfo[] => {
  const projected: ProtocolPrInfo[] = [];
  for (const pr of lists.flatMap((list) => list.prs)) {
    const exactBranch =
      pr.workspace_id === session.workspaceId && pr.head === session.branch;
    const mentioned = mentions?.urls.has(pr.url.replace(/\/$/u, "").toLowerCase()) === true
      || (pr.workspace_id === session.workspaceId
        && mentions?.numbers.has(pr.number) === true);
    if (exactBranch || mentioned || known.some((candidate) => samePullRequest(candidate, pr))) {
      projected.push(pr);
    }
  }
  for (const pr of known) {
    if (!projected.some((candidate) => samePullRequest(candidate, pr))) {
      projected.push(pr);
    }
  }
  projected.sort((left, right) => {
    const leftCreated = left.workspace_id === session.workspaceId && left.head === session.branch;
    const rightCreated = right.workspace_id === session.workspaceId && right.head === session.branch;
    const leftKnown = known.findIndex((candidate) => samePullRequest(candidate, left));
    const rightKnown = known.findIndex((candidate) => samePullRequest(candidate, right));
    const leftPriority = leftCreated ? 0 : leftKnown >= 0 ? leftKnown + 1 : Number.MAX_SAFE_INTEGER;
    const rightPriority = rightCreated ? 0 : rightKnown >= 0 ? rightKnown + 1 : Number.MAX_SAFE_INTEGER;
    return leftPriority - rightPriority
      || Number(right.state === "open") - Number(left.state === "open")
      || right.number - left.number;
  });
  return Object.freeze(projected);
};

export class AppStore {
  readonly #maxThreadViews: number;
  readonly #revision = createSignal(0);
  readonly #sessionMetadata = new Map<string, ProtocolSession>();
  readonly #deletedSessions = new Map<string, number | undefined>();
  readonly #sessionSummaries = new Map<string, ProtocolSessionSummary>();
  readonly #seenSessionCursors = new Map<string, number>();
  #sessionSummaryCursor = 0;
  #sessionSummaryInitialized = false;
  readonly #workspaces = new Map<string, ProtocolWorkspace>();
  readonly #threads = new Map<string, ProtocolThread>();
  readonly #threadStatuses = new Map<string, ProtocolThreadStatus>();
  readonly #seenThreadCursors = new Map<string, number>();
  readonly #initializedThreadSessions = new Set<string>();
  readonly #initializedThreadStatusSessions = new Set<string>();
  readonly #sessionUsageRevisions = new Map<string, number>();
  readonly #githubPullRequests = new Map<string, GithubPullRequestSnapshot>();
  readonly #sessionPullRequests = new Map<string, readonly ProtocolPrInfo[]>();
  readonly #sessionPullRequestMentions = new Map<string, SessionPullRequestMentions>();
  #serverProjectionCursor = 0;
  readonly #threadViews = new Map<string, ThreadViewModel>();
  readonly #threadTodoEvents = new Map<string, readonly ProtocolTodoItem[]>();
  readonly #serverInfo = createSignal<ProtocolServerInfo | undefined>(undefined);
  readonly #automationRevision = createSignal(0);
  readonly #gitWorktreeSettings = createSignal<GitWorktreeSettingsSnapshot | undefined>(
    undefined,
  );

  readonly serverInfo: ReadonlySignal<ProtocolServerInfo | undefined> = this.#serverInfo;
  /** Edge-triggered automation events are exposed as a monotonic invalidation
   * token. The screen refetches the authoritative list while retaining its
   * 15-second poll as a recovery fallback. */
  readonly automationRevision: ReadonlySignal<number> = this.#automationRevision;
  readonly gitWorktreeSettings: ReadonlySignal<GitWorktreeSettingsSnapshot | undefined> =
    this.#gitWorktreeSettings;

  constructor(options: { readonly maxThreadViews?: number } = {}) {
    this.#maxThreadViews = Math.max(1, options.maxThreadViews ?? 8);
  }

  readonly workspaces: ReadonlySignal<readonly ProtocolWorkspace[]> = createComputed(() => {
    this.#revision.get();
    return [...this.#workspaces.values()].sort(
      (left, right) => left.name.localeCompare(right.name) || left.id.localeCompare(right.id),
    );
  });

  readonly sessions: ReadonlySignal<readonly SessionListItem[]> = createComputed(() => {
    this.#revision.get();
    const ids = new Set([
      ...this.#sessionMetadata.keys(),
      ...this.#sessionSummaries.keys(),
    ]);
    const sessions = [...ids]
      .map((id) => {
        const metadata = this.#sessionMetadata.get(id);
        const summary =
          this.#sessionSummaries.get(id) ??
          (metadata === undefined ? undefined : fallbackSessionSummary(metadata));
        return summary === undefined
          ? undefined
          : toListItem(
              summary,
              metadata,
              this.#seenSessionCursors.get(id) ?? summary.latest_cursor,
            );
      })
      .filter((session): session is SessionListItem => session !== undefined);
    return sortInboxSessions(sessions);
  });

  readonly githubPullRequests: ReadonlySignal<readonly GithubPullRequestSnapshot[]> =
    createComputed(() => {
      this.#revision.get();
      return [...this.#githubPullRequests.values()].sort((left, right) =>
        left.pullRequests.host.localeCompare(right.pullRequests.host));
    });

  replaceSessionMetadata(sessions: readonly ProtocolSession[]): void {
    this.#sessionMetadata.clear();
    for (const session of sessions) {
      if (this.#deletedSessions.has(session.id)) continue;
      this.#sessionMetadata.set(session.id, session);
    }
    this.#touch();
  }

  upsertSessionMetadata(session: ProtocolSession): void {
    if (this.#deletedSessions.has(session.id)) return;
    this.#sessionMetadata.set(session.id, session);
    const summary = this.#sessionSummaries.get(session.id);
    if (summary !== undefined && session.archived !== undefined) {
      this.#sessionSummaries.set(session.id, {
        ...summary,
        archived: session.archived,
      });
    }
    this.#touch();
  }

  removeSession(sessionId: string, cursor?: number): void {
    const hadTombstone = this.#deletedSessions.has(sessionId);
    const previous = this.#deletedSessions.get(sessionId);
    if (!hadTombstone) {
      this.#deletedSessions.set(sessionId, cursor);
    } else if (cursor !== undefined && (previous === undefined || cursor > previous)) {
      // A local HTTP deletion starts as an unbounded tombstone. Its later
      // durable delete edge supplies the ordering cursor that can eventually
      // prove a same-id `session.created` event is genuinely newer.
      this.#deletedSessions.set(sessionId, cursor);
    }
    this.#sessionMetadata.delete(sessionId);
    this.#sessionSummaries.delete(sessionId);
    this.#seenSessionCursors.delete(sessionId);
    this.#sessionPullRequests.delete(sessionId);
    this.#sessionPullRequestMentions.delete(sessionId);
    this.#sessionUsageRevisions.delete(sessionId);
    for (const [threadId, thread] of this.#threads) {
      if (thread.session_id === sessionId) {
        this.#threads.delete(threadId);
        this.#threadViews.delete(threadId);
        this.#threadTodoEvents.delete(threadId);
        this.#threadStatuses.delete(threadId);
        this.#seenThreadCursors.delete(threadId);
      }
    }
    // Status snapshots can arrive before (or without) thread metadata. Purge
    // those tombstoned IDs independently so they cannot retain unread state,
    // folded views, or todo projections after the session is gone.
    for (const [threadId, status] of this.#threadStatuses) {
      if (status.session_id !== sessionId) continue;
      this.#threadStatuses.delete(threadId);
      this.#seenThreadCursors.delete(threadId);
      this.#threadViews.delete(threadId);
      this.#threadTodoEvents.delete(threadId);
    }
    this.#initializedThreadSessions.delete(sessionId);
    this.#initializedThreadStatusSessions.delete(sessionId);
    this.#touch();
  }

  replaceWorkspaces(workspaces: readonly ProtocolWorkspace[]): void {
    this.#workspaces.clear();
    for (const workspace of workspaces) this.#workspaces.set(workspace.id, workspace);
    this.#touch();
  }

  replaceServerInfo(info: ProtocolServerInfo): void {
    this.#serverInfo.set(info);
  }

  /** Replace cold-start server-owned projections without replaying retained
   * server history. Host event cursors still win independently if a live SSE
   * update raced this response. */
  replaceServerProjection(
    cursor: number,
    projection: ProtocolServerProjection,
  ): boolean {
    if (cursor < this.#serverProjectionCursor) return false;
    this.#serverProjectionCursor = cursor;

    const newerHosts = [...this.#githubPullRequests.entries()]
      .filter(([, snapshot]) => snapshot.cursor > cursor);
    this.#githubPullRequests.clear();
    for (const [host, snapshot] of newerHosts) {
      this.#githubPullRequests.set(host, snapshot);
    }
    for (const snapshot of projection.github_pull_requests) {
      const host = snapshot.pull_requests.host;
      const current = this.#githubPullRequests.get(host);
      if (current !== undefined && current.cursor > snapshot.cursor) continue;
      const pullRequests: ProtocolGithubPrList = {
        ...snapshot.pull_requests,
        prs: [...snapshot.pull_requests.prs],
      };
      Object.freeze(pullRequests.prs);
      Object.freeze(pullRequests);
      this.#githubPullRequests.set(host, Object.freeze({
        cursor: snapshot.cursor,
        refreshedAt: snapshot.refreshed_at,
        pullRequests,
      }));
    }

    this.#sessionPullRequests.clear();
    for (const session of projection.session_pull_requests) {
      if (this.#deletedSessions.has(session.session_id)) continue;
      this.#sessionPullRequests.set(
        session.session_id,
        Object.freeze([...session.prs]),
      );
    }
    this.replaceGitWorktreeSettings(cursor, projection.git_worktree_settings);
    this.#touch();
    return true;
  }

  replaceThreadsForSession(
    sessionId: string,
    threads: readonly ProtocolThread[],
  ): void {
    if (this.#deletedSessions.has(sessionId)) return;
    this.#initializedThreadSessions.add(sessionId);
    const nextThreadIds = new Set(threads.map((thread) => thread.id));
    for (const [threadId, thread] of this.#threads) {
      if (thread.session_id === sessionId) {
        this.#threads.delete(threadId);
        if (!nextThreadIds.has(threadId)) {
          this.#threadViews.delete(threadId);
          this.#threadTodoEvents.delete(threadId);
          this.#threadStatuses.delete(threadId);
          this.#seenThreadCursors.delete(threadId);
        }
      }
    }
    // Status snapshots are independent from thread metadata and can arrive
    // first. Once the authoritative thread list arrives, remove status-only
    // orphans that the metadata loop above could not discover.
    for (const [threadId, status] of this.#threadStatuses) {
      if (status.session_id !== sessionId || nextThreadIds.has(threadId)) continue;
      this.#threadStatuses.delete(threadId);
      this.#seenThreadCursors.delete(threadId);
      this.#threadViews.delete(threadId);
      this.#threadTodoEvents.delete(threadId);
    }
    for (const thread of threads) this.#storeThreadSnapshot(thread);
    this.#touch();
  }

  upsertThread(thread: ProtocolThread): void {
    if (this.#deletedSessions.has(thread.session_id)) return;
    this.#storeThreadSnapshot(thread);
    this.#touch();
  }

  replaceThreadStatusesForSession(
    sessionId: string,
    statuses: readonly ProtocolThreadStatus[],
  ): void {
    if (this.#deletedSessions.has(sessionId)) return;
    const firstSnapshot = !this.#initializedThreadStatusSessions.has(sessionId);
    const threadListInitialized = this.#initializedThreadSessions.has(sessionId);
    const knownThreadIds = new Set(
      [...this.#threads.values()]
        .filter((thread) => thread.session_id === sessionId)
        .map((thread) => thread.id),
    );
    // A list response has no collection cursor. It cannot prove that a status
    // received from newer SSE traffic was deleted; thread-list replacement is
    // the authoritative place that removes status for deleted threads.
    for (const status of statuses) {
      if (
        status.session_id !== sessionId
        || (threadListInitialized && !knownThreadIds.has(status.thread_id))
      ) continue;
      const current = this.#threadStatuses.get(status.thread_id);
      if (current !== undefined && current.latest_cursor > status.latest_cursor) continue;
      this.#recordSessionUsageCompletion(current, status);
      this.#threadStatuses.set(status.thread_id, status);
      if (firstSnapshot) {
        this.#seenThreadCursors.set(status.thread_id, status.latest_cursor);
      } else if (!this.#seenThreadCursors.has(status.thread_id)) {
        this.#seenThreadCursors.set(
          status.thread_id,
          status.active || status.outcome === "running" ? status.latest_cursor : 0,
        );
      }
    }
    this.#initializedThreadStatusSessions.add(sessionId);
    this.#touch();
  }

  threadsForSession(sessionId: string): readonly ProtocolThread[] {
    this.#revision.get();
    return [...this.#threads.values()]
      .filter((thread) => thread.session_id === sessionId)
      .sort(
        (left, right) =>
          left.created_at.localeCompare(right.created_at) || left.id.localeCompare(right.id),
      );
  }

  /** Every thread identity currently associated with a session, including a
   * status that arrived before its metadata. Session deletion uses this
   * synchronous snapshot before purging the projections. */
  sessionThreadIds(sessionId: string): readonly string[] {
    this.#revision.get();
    const ids = new Set<string>();
    for (const thread of this.#threads.values()) {
      if (thread.session_id === sessionId) ids.add(thread.id);
    }
    for (const status of this.#threadStatuses.values()) {
      if (status.session_id === sessionId) ids.add(status.thread_id);
    }
    return Object.freeze([...ids]);
  }

  /** Monotonic invalidation token for authoritative session usage totals.
   * Thread status events are server-scoped, so background-thread completions
   * advance this even when that thread's transcript stream is not open. */
  sessionUsageRevision(sessionId: string): number {
    this.#revision.get();
    return this.#sessionUsageRevisions.get(sessionId) ?? 0;
  }

  thread(threadId: string): ProtocolThread | undefined {
    this.#revision.get();
    return this.#threads.get(threadId);
  }

  threadIndicatorState(threadId: string): ThreadIndicatorState {
    this.#revision.get();
    const status = this.#threadStatuses.get(threadId);
    if (status === undefined) {
      return { active: false, attention: "none", outcome: "idle", unread: false };
    }
    const unread = !status.active
      && (status.outcome === "succeeded" || status.outcome === "failed")
      && status.latest_cursor > (this.#seenThreadCursors.get(threadId) ?? status.latest_cursor);
    return {
      active: status.active,
      attention: status.attention,
      outcome: status.outcome,
      unread,
    };
  }

  threadStatus(threadId: string): ProtocolThreadStatus | undefined {
    this.#revision.get();
    return this.#threadStatuses.get(threadId);
  }

  session(sessionId: string): SessionListItem | undefined {
    this.#revision.get();
    const metadata = this.#sessionMetadata.get(sessionId);
    const summary =
      this.#sessionSummaries.get(sessionId) ??
      (metadata === undefined ? undefined : fallbackSessionSummary(metadata));
    return summary === undefined
      ? undefined
      : toListItem(
          summary,
          metadata,
          this.#seenSessionCursors.get(sessionId) ?? summary.latest_cursor,
        );
  }

  /** Full protocol metadata is exposed only for protocol-backed client
   * workflows such as containing a model-authored file link to its session
   * worktree. Render-facing lists continue to use SessionListItem. */
  sessionMetadata(sessionId: string): ProtocolSession | undefined {
    this.#revision.get();
    return this.#sessionMetadata.get(sessionId);
  }

  isSessionTombstoned(sessionId: string): boolean {
    this.#revision.get();
    return this.#deletedSessions.has(sessionId);
  }

  /** Shared session PR selector used by the right pane and navigation badge.
   * Account snapshots supply live status while per-session results preserve
   * associations the account feed cannot infer from branch identity alone. */
  sessionPullRequests(sessionId: string): readonly ProtocolPrInfo[] {
    this.#revision.get();
    const session = this.session(sessionId);
    if (session === undefined) return Object.freeze([]);
    return projectSessionPullRequests(
      session,
      [...this.#githubPullRequests.values()].map(({ pullRequests }) => pullRequests),
      this.#sessionPullRequests.get(sessionId) ?? [],
      this.#sessionPullRequestMentions.get(sessionId),
    );
  }

  replaceSessionPullRequests(
    sessionId: string,
    pullRequests: readonly ProtocolPrInfo[],
  ): void {
    if (this.#deletedSessions.has(sessionId)) return;
    this.#sessionPullRequests.set(sessionId, Object.freeze([...pullRequests]));
    this.#touch();
  }

  clearSessionPullRequests(sessionId: string): void {
    if (!this.#sessionPullRequests.delete(sessionId)) return;
    this.#touch();
  }

  threadView(threadId: string): ThreadViewModel {
    this.#revision.get();
    let view = this.#threadViews.get(threadId);
    if (view === undefined) {
      while (this.#threadViews.size >= this.#maxThreadViews) {
        const oldest = this.#threadViews.keys().next().value as string | undefined;
        if (oldest === undefined) break;
        this.#threadViews.delete(oldest);
      }
      view = new ThreadViewModel();
      const todoEvent = this.#threadTodoEvents.get(threadId);
      const todos = todoEvent ?? this.#threads.get(threadId)?.todos;
      if (todos !== undefined) view.replaceTodos(todos);
    } else {
      this.#threadViews.delete(threadId);
    }
    this.#threadViews.set(threadId, view);
    return view;
  }

  /** Merge the server-folded live tail into any retained prefetched history. */
  replaceThreadViewSnapshot(
    threadId: string,
    cursor: number,
    snapshot: ProtocolThreadViewSnapshot,
  ): boolean {
    const current = this.#threadViews.get(threadId);
    if (current !== undefined && current.cursor > cursor) return false;
    for (const [cachedThreadId, cachedView] of this.#threadViews) {
      if (cachedThreadId !== threadId) {
        cachedView.trimHistory(MAX_CACHED_THREAD_HISTORY_ITEMS);
      }
    }
    this.#threadViews.delete(threadId);
    while (this.#threadViews.size >= this.#maxThreadViews) {
      const oldest = this.#threadViews.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      this.#threadViews.delete(oldest);
    }
    const view = current ?? new ThreadViewModel();
    view.mergeTailSnapshot(cursor, snapshot);
    if (snapshot.todos === undefined) {
      const preserved = this.#threadTodoEvents.get(threadId)
        ?? this.#threads.get(threadId)?.todos;
      if (preserved !== undefined) view.replaceTodos(preserved);
    }
    this.#threadViews.set(threadId, view);

    if (snapshot.todos !== undefined) {
      const todos = view.todos.map((todo) => ({ ...todo }));
      this.#threadTodoEvents.set(threadId, todos);
      const thread = this.#threads.get(threadId);
      if (thread !== undefined) this.#threads.set(threadId, { ...thread, todos });
    }
    this.#touch();
    return true;
  }

  replaceThreadToolDetails(
    threadId: string,
    details: ProtocolThreadToolDetails,
  ): boolean {
    const view = this.#threadViews.get(threadId);
    if (view === undefined || !view.replaceToolDetails(details)) return false;
    this.#touch();
    return true;
  }

  /** Prepend one contiguous folded page while retaining newer live state. */
  prependThreadViewSnapshot(
    threadId: string,
    snapshot: ProtocolThreadViewSnapshot,
  ): boolean {
    const view = this.threadView(threadId);
    if (!view.prependSnapshot(snapshot)) return false;
    this.#touch();
    return true;
  }

  applyThreadEvent(threadId: string, envelope: ProtocolEventEnvelope): boolean {
    return this.applyThreadEvents(threadId, [envelope]);
  }

  resolveApprovalOptimistically(
    threadId: string,
    callId: string,
    decision: "approve" | "always_approve" | "deny",
  ): boolean {
    const tool = this.#threadViews.get(threadId)?.findTool(callId);
    if (tool?.status !== "awaiting-approval") return false;
    tool.status = decision === "deny" ? "denied" : "running";
    this.#touch();
    return true;
  }

  /** Fold a replay burst while invalidating Lit's shared store signal once.
   * The native client historically batched persisted history for the same
   * reason: a reconnect should not schedule one complete chat render for
   * every historical envelope. */
  applyThreadEvents(
    threadId: string,
    envelopes: readonly ProtocolEventEnvelope[],
  ): boolean {
    if (envelopes.length === 0) return false;
    const view = this.threadView(threadId);
    let changed = false;
    for (const envelope of envelopes) {
      if (envelope.type === "thread.todos_updated") {
        const todos = envelope.todos.map((todo) => ({ ...todo }));
        this.#threadTodoEvents.set(threadId, todos);
        const thread = this.#threads.get(threadId);
        if (thread !== undefined) this.#threads.set(threadId, { ...thread, todos });
      }
      changed = view.apply(envelope) || changed;
    }
    if (changed) this.#touch();
    return changed;
  }

  replaceThreadQueue(threadId: string, prompts: readonly QueuedPrompt[]): void {
    this.threadView(threadId).replaceQueue(prompts);
    this.#touch();
  }

  replaceSessionSummaries(
    summaries: readonly ProtocolSessionSummary[],
    cursor = 0,
  ): void {
    if (cursor < this.#sessionSummaryCursor) return;
    const firstSnapshot = !this.#sessionSummaryInitialized;
    const visibleSummaries = summaries.filter(
      ({ session_id }) => !this.#deletedSessions.has(session_id),
    );
    const nextIds = new Set(visibleSummaries.map(({ session_id }) => session_id));
    for (const sessionId of this.#seenSessionCursors.keys()) {
      if (!nextIds.has(sessionId)) this.#seenSessionCursors.delete(sessionId);
    }
    this.#sessionSummaries.clear();
    for (const summary of visibleSummaries) {
      this.#sessionSummaries.set(summary.session_id, summary);
      if (firstSnapshot) {
        this.#seenSessionCursors.set(summary.session_id, summary.latest_cursor);
      } else if (!this.#seenSessionCursors.has(summary.session_id)) {
        this.#seenSessionCursors.set(
          summary.session_id,
          summary.active || summary.outcome === "running" ? summary.latest_cursor : 0,
        );
      }
    }
    this.#sessionSummaryInitialized = true;
    this.#sessionSummaryCursor = cursor;
    this.#touch();
  }

  /** Apply a known generated event. Returns true when session metadata should
   * be refreshed because a lifecycle event may have changed title/branch. */
  applyServerEvent(envelope: ProtocolEventEnvelope): boolean {
    switch (envelope.type) {
      case "session.activity": {
        // Protocol 2.4/early-2.5 servers publish activity without the newer
        // full-replacement summary event. Keep their running indicators live;
        // newer servers immediately supersede this compatibility projection
        // with the adjacent session.summary_updated event.
        if (envelope.cursor <= this.#sessionSummaryCursor) return false;
        if (this.#deletedSessions.has(envelope.session_id)) return false;
        const metadata = this.#sessionMetadata.get(envelope.session_id);
        const previous = this.#sessionSummaries.get(envelope.session_id)
          ?? (metadata === undefined ? undefined : fallbackSessionSummary(metadata));
        if (previous === undefined) return true;
        this.#sessionSummaryCursor = envelope.cursor;
        this.#sessionSummaries.set(envelope.session_id, {
          ...previous,
          active: envelope.active,
          outcome: envelope.active
            ? "running"
            : previous.outcome === "running" ? "idle" : previous.outcome,
          latest_cursor: Math.max(previous.latest_cursor, envelope.cursor),
          updated_at: envelope.ts,
        });
        this.#touch();
        return false;
      }
      case "session.summary_updated": {
        if (envelope.cursor <= this.#sessionSummaryCursor) return false;
        this.#sessionSummaryCursor = envelope.cursor;
        if (envelope.summary === null) {
          this.removeSession(envelope.session_id, envelope.cursor);
          return false;
        }
        if (this.#deletedSessions.has(envelope.session_id)) {
          return false;
        } else {
          const previous = this.#sessionSummaries.get(envelope.session_id);
          if (!this.#seenSessionCursors.has(envelope.session_id)) {
            this.#seenSessionCursors.set(
              envelope.session_id,
              envelope.summary.active || envelope.summary.outcome === "running"
                ? envelope.summary.latest_cursor
                : 0,
            );
          } else {
            const seen = this.#seenSessionCursors.get(envelope.session_id) ?? 0;
            const alreadyUnread = previous !== undefined
              && terminalSummary(previous)
              && previous.latest_cursor > seen;
            if (!alreadyUnread && !liveTerminalTransition(previous, envelope.summary)) {
              this.#seenSessionCursors.set(
                envelope.session_id,
                envelope.summary.latest_cursor,
              );
            }
          }
          this.#sessionSummaries.set(envelope.session_id, envelope.summary);
        }
        this.#touch();
        return !this.#sessionMetadata.has(envelope.session_id);
      }
      case "thread.status_updated": {
        const next = envelope.status;
        if (this.#deletedSessions.has(next.session_id)) return false;
        const previous = this.#threadStatuses.get(next.thread_id);
        if (previous !== undefined && previous.latest_cursor >= next.latest_cursor) return false;
        this.#initializedThreadStatusSessions.add(next.session_id);
        if (!this.#seenThreadCursors.has(next.thread_id)) {
          this.#seenThreadCursors.set(
            next.thread_id,
            next.active || next.outcome === "running" ? next.latest_cursor : 0,
          );
        } else {
          const seen = this.#seenThreadCursors.get(next.thread_id) ?? 0;
          const alreadyUnread = previous !== undefined
            && terminalSummary(previous)
            && previous.latest_cursor > seen;
          if (!alreadyUnread && !liveTerminalTransition(previous, next)) {
            this.#seenThreadCursors.set(next.thread_id, next.latest_cursor);
          }
        }
        this.#recordSessionUsageCompletion(previous, next);
        this.#threadStatuses.set(next.thread_id, next);
        this.#touch();
        return false;
      }
      case "session.created": {
        const deletedAt = this.#deletedSessions.get(envelope.session_id);
        if (this.#deletedSessions.has(envelope.session_id)) {
          if (deletedAt === undefined || envelope.cursor <= deletedAt) return false;
          this.#deletedSessions.delete(envelope.session_id);
          this.#touch();
        }
        return true;
      }
      case "session.updated":
        if (this.#deletedSessions.has(envelope.session_id)) return false;
        return true;
      case "workspace.registered":
      case "workspace.closed":
        return true;
      case "session.deleted":
        this.removeSession(envelope.session_id, envelope.cursor);
        return false;
      case "session.pr_mentioned": {
        if (this.#deletedSessions.has(envelope.session_id)) return false;
        const current = this.#sessionPullRequestMentions.get(envelope.session_id);
        const urls = new Set(current?.urls ?? []);
        const numbers = new Set(current?.numbers ?? []);
        if (envelope.url == null) numbers.add(envelope.number);
        else urls.add(envelope.url.replace(/\/$/u, "").toLowerCase());
        this.#sessionPullRequestMentions.set(envelope.session_id, Object.freeze({ urls, numbers }));
        this.#touch();
        return false;
      }
      case "github.pull_requests_updated": {
        const host = envelope.pull_requests.host;
        const current = this.#githubPullRequests.get(host);
        if (current !== undefined && envelope.cursor <= current.cursor) return false;
        const pullRequests: ProtocolGithubPrList = {
          ...envelope.pull_requests,
          prs: [...envelope.pull_requests.prs],
        };
        Object.freeze(pullRequests.prs);
        Object.freeze(pullRequests);
        this.#githubPullRequests.set(host, Object.freeze({
          cursor: envelope.cursor,
          refreshedAt: envelope.ts,
          pullRequests,
        }));
        this.#touch();
        return false;
      }
      case "settings.git_worktrees_updated":
        this.replaceGitWorktreeSettings(envelope.cursor, envelope.settings);
        return false;
      case "automation.fired":
        this.#automationRevision.set(this.#automationRevision.get() + 1);
        // Successful automation runs create sessions. The replacement summary
        // normally arrives adjacent to this event, but refetching metadata also
        // covers older servers and failed/missing summary delivery.
        return true;
      case "server.connectivity_changed": {
        const current = this.#serverInfo.get();
        if (current !== undefined && current.online !== envelope.online) {
          this.#serverInfo.set(Object.freeze({ ...current, online: envelope.online }));
        }
        return false;
      }
      default:
        return false;
    }
  }

  /** Mark the current replacement summary as presented by this frontend.
   * This intentionally never changes durable protocol outcome state. */
  markSessionRead(sessionId: string): boolean {
    const summary = this.#sessionSummaries.get(sessionId);
    if (summary === undefined) return false;
    const seen = this.#seenSessionCursors.get(sessionId) ?? 0;
    if (seen >= summary.latest_cursor) return false;
    this.#seenSessionCursors.set(sessionId, summary.latest_cursor);
    this.#touch();
    return true;
  }

  markThreadRead(threadId: string): boolean {
    const status = this.#threadStatuses.get(threadId);
    if (status === undefined) return false;
    const seen = this.#seenThreadCursors.get(threadId) ?? 0;
    if (seen >= status.latest_cursor) return false;
    this.#seenThreadCursors.set(threadId, status.latest_cursor);
    this.#touch();
    return true;
  }

  /** Apply an HTTP snapshot or durable event only when it is not older than
   * the current projection. The cursor ordering closes the response-versus-
   * SSE race while a title-model install publishes progress. */
  replaceGitWorktreeSettings(
    cursor: number,
    settings: ProtocolGitWorktreeSettings,
  ): boolean {
    const current = this.#gitWorktreeSettings.get();
    if (current !== undefined && current.cursor > cursor) return false;
    const titleModel = Object.freeze({ ...settings.title_model });
    const frozen = Object.freeze({ ...settings, title_model: titleModel });
    this.#gitWorktreeSettings.set(Object.freeze({ cursor, settings: frozen }));
    return true;
  }

  #touch(): void {
    this.#revision.set(this.#revision.get() + 1);
  }

  #recordSessionUsageCompletion(
    previous: ProtocolThreadStatus | undefined,
    next: ProtocolThreadStatus,
  ): void {
    if (next.completed_at == null || next.completed_at === previous?.completed_at) return;
    const revision = this.#sessionUsageRevisions.get(next.session_id) ?? 0;
    this.#sessionUsageRevisions.set(next.session_id, revision + 1);
  }

  #storeThreadSnapshot(thread: ProtocolThread): void {
    if (this.#deletedSessions.has(thread.session_id)) return;
    const todoEvent = this.#threadTodoEvents.get(thread.id);
    const todos = todoEvent ?? thread.todos;
    const stored = todos === undefined
      ? thread
      : { ...thread, todos: todos.map((todo) => ({ ...todo })) };
    this.#threads.set(thread.id, stored);
    if (todoEvent === undefined && thread.todos !== undefined) {
      this.#threadViews.get(thread.id)?.replaceTodos(thread.todos);
    }
  }
}

export const createGalleryStore = (): AppStore => {
  const store = new AppStore();
  store.replaceSessionMetadata([
    {
      id: "se-visual-parity",
      workspace_id: "ws-trouve",
      title: "Preserve the existing frontend",
      branch: "trouve/web-frontend",
      worktree_path: "/tmp/trouve/web-frontend",
      base_ref: "main",
      created_at: "2026-08-01T14:00:00Z",
    },
    {
      id: "se-approval",
      workspace_id: "ws-trouve",
      title: "Review protocol changes",
      branch: "trouve/session-summaries",
      worktree_path: "/tmp/trouve/session-summaries",
      base_ref: "main",
      created_at: "2026-08-01T13:00:00Z",
    },
    {
      id: "se-finished",
      workspace_id: "ws-search",
      title: "Tune search benchmarks",
      branch: "trouve/search-bench",
      worktree_path: "/tmp/trouve/search-bench",
      base_ref: "main",
      created_at: "2026-08-01T12:00:00Z",
    },
  ]);
  store.replaceSessionSummaries([
    {
      session_id: "se-visual-parity",
      workspace_id: "ws-trouve",
      archived: false,
      active: true,
      attention: "none",
      outcome: "running",
      latest_thread_id: "th-code",
      latest_cursor: 42,
      updated_at: "2026-08-01T14:30:00Z",
    },
    {
      session_id: "se-approval",
      workspace_id: "ws-trouve",
      archived: false,
      active: false,
      attention: "approval",
      outcome: "idle",
      latest_thread_id: "th-review",
      latest_cursor: 40,
      updated_at: "2026-08-01T14:20:00Z",
    },
    {
      session_id: "se-finished",
      workspace_id: "ws-search",
      archived: false,
      active: false,
      attention: "none",
      outcome: "succeeded",
      latest_cursor: 38,
      updated_at: "2026-08-01T14:10:00Z",
    },
  ]);
  // Gallery fixtures intentionally exercise the transient unread-completion
  // treatment; a cold snapshot alone is considered already seen.
  store.applyServerEvent({
    cursor: 43,
    scope: "server",
    ts: "2026-08-01T14:31:00Z",
    type: "session.summary_updated",
    session_id: "se-finished",
    summary: {
      session_id: "se-finished",
      workspace_id: "ws-search",
      archived: false,
      active: false,
      attention: "none",
      outcome: "succeeded",
      latest_cursor: 43,
      updated_at: "2026-08-01T14:31:00Z",
    },
  });
  return store;
};
