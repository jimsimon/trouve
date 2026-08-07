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
  ProtocolThreadViewSnapshot,
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

export interface SessionPullRequestIdentity {
  readonly workspaceId: string;
  readonly branch: string;
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

const terminalSummary = (summary: ProtocolSessionSummary): boolean =>
  !summary.active
  && (summary.outcome === "succeeded" || summary.outcome === "failed");

/** A live full-replacement event represents a newly completed turn only when
 * it crosses into a terminal state (or changes terminal outcome). Other
 * summary updates may advance latest_cursor for metadata/attention changes and
 * must not resurrect a terminal badge the user already saw. */
const liveTerminalTransition = (
  previous: ProtocolSessionSummary | undefined,
  next: ProtocolSessionSummary,
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
): SessionListItem => ({
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
  state: visualState(
    summary,
    !summary.active
      && (summary.outcome === "succeeded" || summary.outcome === "failed")
      && summary.latest_cursor > seenCursor,
  ),
  unread: !summary.active
    && (summary.outcome === "succeeded" || summary.outcome === "failed")
    && summary.latest_cursor > seenCursor,
});

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
): readonly ProtocolPrInfo[] => {
  const projected: ProtocolPrInfo[] = [];
  for (const pr of lists.flatMap((list) => list.prs)) {
    const exactBranch =
      pr.workspace_id === session.workspaceId && pr.head === session.branch;
    if (exactBranch || known.some((candidate) => samePullRequest(candidate, pr))) {
      projected.push(pr);
    }
  }
  for (const pr of known) {
    if (!projected.some((candidate) => samePullRequest(candidate, pr))) {
      projected.push(pr);
    }
  }
  projected.sort((left, right) =>
    Number(right.state === "open") - Number(left.state === "open") ||
    right.number - left.number);
  return Object.freeze(projected);
};

export class AppStore {
  readonly #maxThreadViews: number;
  readonly #revision = createSignal(0);
  readonly #sessionMetadata = new Map<string, ProtocolSession>();
  readonly #sessionSummaries = new Map<string, ProtocolSessionSummary>();
  readonly #seenSessionCursors = new Map<string, number>();
  #sessionSummaryCursor = 0;
  #sessionSummaryInitialized = false;
  readonly #workspaces = new Map<string, ProtocolWorkspace>();
  readonly #threads = new Map<string, ProtocolThread>();
  readonly #githubPullRequests = new Map<string, GithubPullRequestSnapshot>();
  readonly #sessionPullRequests = new Map<string, readonly ProtocolPrInfo[]>();
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
    for (const session of sessions) this.#sessionMetadata.set(session.id, session);
    this.#touch();
  }

  upsertSessionMetadata(session: ProtocolSession): void {
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

  removeSession(sessionId: string): void {
    this.#sessionMetadata.delete(sessionId);
    this.#sessionSummaries.delete(sessionId);
    this.#seenSessionCursors.delete(sessionId);
    this.#sessionPullRequests.delete(sessionId);
    for (const [threadId, thread] of this.#threads) {
      if (thread.session_id === sessionId) {
        this.#threads.delete(threadId);
        this.#threadViews.delete(threadId);
        this.#threadTodoEvents.delete(threadId);
      }
    }
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
    const nextThreadIds = new Set(threads.map((thread) => thread.id));
    for (const [threadId, thread] of this.#threads) {
      if (thread.session_id === sessionId) {
        this.#threads.delete(threadId);
        if (!nextThreadIds.has(threadId)) {
          this.#threadViews.delete(threadId);
          this.#threadTodoEvents.delete(threadId);
        }
      }
    }
    for (const thread of threads) this.#storeThreadSnapshot(thread);
    this.#touch();
  }

  upsertThread(thread: ProtocolThread): void {
    this.#storeThreadSnapshot(thread);
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

  thread(threadId: string): ProtocolThread | undefined {
    this.#revision.get();
    return this.#threads.get(threadId);
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
    );
  }

  replaceSessionPullRequests(
    sessionId: string,
    pullRequests: readonly ProtocolPrInfo[],
  ): void {
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

  /** Atomically install the server-folded transcript tail at its SSE cursor. */
  replaceThreadViewSnapshot(
    threadId: string,
    cursor: number,
    snapshot: ProtocolThreadViewSnapshot,
  ): boolean {
    const current = this.#threadViews.get(threadId);
    if (current !== undefined && current.cursor > cursor) return false;
    this.#threadViews.delete(threadId);
    while (this.#threadViews.size >= this.#maxThreadViews) {
      const oldest = this.#threadViews.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      this.#threadViews.delete(oldest);
    }
    const view = ThreadViewModel.fromSnapshot(cursor, snapshot);
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
   * The retained Slint controller batches persisted history for the same
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
    const nextIds = new Set(summaries.map(({ session_id }) => session_id));
    for (const sessionId of this.#seenSessionCursors.keys()) {
      if (!nextIds.has(sessionId)) this.#seenSessionCursors.delete(sessionId);
    }
    this.#sessionSummaries.clear();
    for (const summary of summaries) {
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
          this.#sessionSummaries.delete(envelope.session_id);
          this.#seenSessionCursors.delete(envelope.session_id);
          this.#sessionMetadata.delete(envelope.session_id);
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
      case "session.created":
      case "session.updated":
      case "session.deleted":
      case "workspace.registered":
      case "workspace.closed":
        return true;
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

  #storeThreadSnapshot(thread: ProtocolThread): void {
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
