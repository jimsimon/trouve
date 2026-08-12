import {
  fallbackSessionSummary,
  type AppStore,
} from "../state/app-store.js";
import { readSignal } from "../state/reactivity.js";
import type { CursorEventStream, SafeStreamDiagnostic } from "./cursor-event-stream.js";
import {
  ProtocolClient,
  ProtocolClientError,
  type ProtocolEventEnvelope,
  type ProtocolIngressEvent,
  type ProtocolCursorSnapshot,
  type ProtocolServerProjection,
  type ProtocolSession,
  type ProtocolSessionSummariesSnapshot,
  type ProtocolSessionSummary,
} from "./protocol-client.js";

interface VisibilitySource {
  readonly visibilityState: DocumentVisibilityState;
  addEventListener(type: "visibilitychange", listener: () => void): void;
  removeEventListener(type: "visibilitychange", listener: () => void): void;
}

interface OnlineSource {
  addEventListener(type: "online", listener: () => void): void;
  removeEventListener(type: "online", listener: () => void): void;
}

export interface ServerReplayScheduler {
  set(delayMs: number, callback: () => void): unknown;
  clear(handle: unknown): void;
}

const browserServerReplayScheduler: ServerReplayScheduler = {
  set: (delayMs, callback) => globalThis.setTimeout(callback, delayMs),
  clear: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

const SERVER_REPLAY_IDLE_FLUSH_MS = 250;

type SessionSummaryBoundary =
  | {
    readonly kind: "atomic";
    readonly snapshot: ProtocolSessionSummariesSnapshot;
  }
  | {
    readonly kind: "metadata-fallback";
    readonly cursor: number;
  };

type ServerProjectionBoundary =
  | {
    readonly kind: "snapshot";
    readonly snapshot: ProtocolCursorSnapshot<ProtocolServerProjection>;
  }
  | { readonly kind: "legacy-replay" };

const materializeSessionSnapshot = (
  boundary: SessionSummaryBoundary,
  sessions: readonly ProtocolSession[],
): ProtocolSessionSummariesSnapshot => boundary.kind === "atomic"
  ? boundary.snapshot
  : {
    cursor: boundary.cursor,
    summaries: sessions.map(fallbackSessionSummary),
  };

type GithubSnapshotEnvelope = Extract<
  ProtocolEventEnvelope,
  { readonly type: "github.pull_requests_updated" }
>;
type GitWorktreeSettingsEnvelope = Extract<
  ProtocolEventEnvelope,
  { readonly type: "settings.git_worktrees_updated" }
>;

/** Keep only the newest durable state replacement from startup history.
 * Snapshot-covered state edges need no replay, and side-effect-only edges such
 * as session notifications must never be replayed as fresh UI activity. */
export class ServerReplayBuffer {
  readonly #github = new Map<string, GithubSnapshotEnvelope>();
  #gitWorktreeSettings: GitWorktreeSettingsEnvelope | undefined;

  push(envelope: ProtocolEventEnvelope): boolean {
    if (envelope.type === "github.pull_requests_updated") {
      const host = envelope.pull_requests.host;
      const current = this.#github.get(host);
      if (current === undefined || envelope.cursor > current.cursor) {
        this.#github.set(host, envelope);
      }
      return true;
    }
    if (envelope.type === "settings.git_worktrees_updated") {
      if (
        this.#gitWorktreeSettings === undefined
        || envelope.cursor > this.#gitWorktreeSettings.cursor
      ) this.#gitWorktreeSettings = envelope;
      return true;
    }
    return false;
  }

  take(): readonly ProtocolEventEnvelope[] {
    const envelopes: ProtocolEventEnvelope[] = [...this.#github.values()];
    if (this.#gitWorktreeSettings !== undefined) {
      envelopes.push(this.#gitWorktreeSettings);
    }
    this.clear();
    return Object.freeze(envelopes.sort((left, right) => left.cursor - right.cursor));
  }

  clear(): void {
    this.#github.clear();
    this.#gitWorktreeSettings = undefined;
  }
}

/** Coordinates the only global protocol ingress: atomic projection snapshot,
 * generated HTTP models, and one resume-capable server SSE stream. */
export class ProtocolIngress {
  readonly #client: ProtocolClient;
  readonly #store: AppStore;
  readonly #visibility: VisibilitySource | undefined;
  readonly #online: OnlineSource | undefined;
  readonly #onDiagnostic: (diagnostic: SafeStreamDiagnostic) => void;
  readonly #onUnknownEvent: (type: string) => void;
  readonly #onKnownEvent: (event: ProtocolEventEnvelope) => void;
  readonly #onSessionSummaries: (
    summaries: readonly ProtocolSessionSummary[],
    cursor: number,
  ) => void;
  readonly #serverReplayScheduler: ServerReplayScheduler;
  readonly #serverReplayIdleFlushMs: number;
  readonly #serverReplay = new ServerReplayBuffer();

  #stream: CursorEventStream<ProtocolIngressEvent> | undefined;
  #started = false;
  #generation = 0;
  #listenersAttached = false;
  #bootstrap: Promise<void> | undefined;
  #projectionRefresh: Promise<void> | undefined;
  #activityReconciliation: Promise<boolean> | undefined;
  #metadataRefresh: Promise<void> | undefined;
  #metadataRefreshPending = false;
  #metadataRevision = 0;
  readonly #threadRefreshes = new Map<string, boolean>();
  #serverReplayTimer: unknown;

  constructor(
    client: ProtocolClient,
    store: AppStore,
    options: {
      readonly visibility?: VisibilitySource;
      readonly online?: OnlineSource;
      readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
      readonly onUnknownEvent?: (type: string) => void;
      readonly onKnownEvent?: (event: ProtocolEventEnvelope) => void;
      readonly onSessionSummaries?: (
        summaries: readonly ProtocolSessionSummary[],
        cursor: number,
      ) => void;
      readonly serverReplayScheduler?: ServerReplayScheduler;
      readonly serverReplayIdleFlushMs?: number;
    } = {},
  ) {
    this.#client = client;
    this.#store = store;
    this.#visibility = options.visibility;
    this.#online = options.online;
    this.#onDiagnostic = options.onDiagnostic ?? (() => undefined);
    this.#onUnknownEvent = options.onUnknownEvent ?? (() => undefined);
    this.#onKnownEvent = options.onKnownEvent ?? (() => undefined);
    this.#onSessionSummaries = options.onSessionSummaries ?? (() => undefined);
    this.#serverReplayScheduler = options.serverReplayScheduler
      ?? browserServerReplayScheduler;
    this.#serverReplayIdleFlushMs = options.serverReplayIdleFlushMs
      ?? SERVER_REPLAY_IDLE_FLUSH_MS;
  }

  start(): Promise<void> {
    if (this.#bootstrap !== undefined) return this.#bootstrap;
    if (this.#started) return Promise.resolve();
    const generation = ++this.#generation;
    this.#started = true;
    this.#attachListeners();
    let bootstrap!: Promise<void>;
    bootstrap = this.#bootstrapGeneration(generation)
      .catch((error: unknown) => {
        if (!this.#isCurrentGeneration(generation)) return;
        this.#started = false;
        this.#stream?.close();
        this.#stream = undefined;
        this.#discardServerReplay();
        throw error;
      })
      .finally(() => {
        if (this.#bootstrap === bootstrap) this.#bootstrap = undefined;
      });
    this.#bootstrap = bootstrap;
    return bootstrap;
  }

  stop(): void {
    this.#generation += 1;
    this.#started = false;
    this.#bootstrap = undefined;
    this.#projectionRefresh = undefined;
    this.#activityReconciliation = undefined;
    this.#metadataRefresh = undefined;
    this.#metadataRefreshPending = false;
    this.#threadRefreshes.clear();
    this.#discardServerReplay();
    this.#detachListeners();
    this.#stream?.close();
    this.#stream = undefined;
  }

  /** Refetch on foreground and after a newly opened transport. A delayed
   * snapshot may not roll the store behind events already accepted by SSE. */
  refreshProjection(): Promise<void> {
    if (!this.#started) return Promise.resolve();
    if (this.#projectionRefresh !== undefined) return this.#projectionRefresh;
    const generation = this.#generation;
    let refresh!: Promise<void>;
    refresh = this.#refreshProjectionGeneration(generation).finally(() => {
      if (this.#projectionRefresh === refresh) {
        this.#projectionRefresh = undefined;
      }
    });
    this.#projectionRefresh = refresh;
    return refresh;
  }

  /** Reconcile only the durable session-activity projection. Desktop sleep
   * inhibition uses this narrow read as a safety net for a missed live event;
   * unlike a full foreground refresh it does not refetch workspace, metadata,
   * or GitHub state. Concurrent timers share one request. */
  reconcileSessionActivity(): Promise<boolean> {
    if (!this.#started) return Promise.resolve(false);
    if (this.#activityReconciliation !== undefined) {
      return this.#activityReconciliation;
    }
    const generation = this.#generation;
    let reconciliation!: Promise<boolean>;
    reconciliation = this.#reconcileSessionActivityGeneration(generation).finally(() => {
      if (this.#activityReconciliation === reconciliation) {
        this.#activityReconciliation = undefined;
      }
    });
    this.#activityReconciliation = reconciliation;
    return reconciliation;
  }

  async #bootstrapGeneration(generation: number): Promise<void> {
    const [info, boundary] = await Promise.all([
      this.#client.serverInfo(),
      this.#sessionSummaryBoundary(),
    ]);
    if (!this.#isCurrentGeneration(generation)) return;
    const [sessions, workspaces, projection] = await Promise.all([
      this.#client.sessions(),
      this.#client.workspaces(),
      this.#serverProjectionBoundary(),
    ]);
    if (!this.#isCurrentGeneration(generation)) return;
    const snapshot = materializeSessionSnapshot(boundary, sessions);
    this.#store.replaceServerInfo(info);
    this.#store.replaceSessionMetadata(sessions);
    this.#store.replaceSessionSummaries(snapshot.summaries, snapshot.cursor);
    this.#onSessionSummaries(snapshot.summaries, snapshot.cursor);
    this.#store.replaceWorkspaces(workspaces);
    if (projection.kind === "snapshot") {
      this.#store.replaceServerProjection(
        projection.snapshot.cursor,
        projection.snapshot.value,
      );
    }

    let skipFirstOpenRefresh = true;
    const stream = await this.#client.serverEvents({
      // Current servers return every durable replacement projection absent
      // from session summaries, so only events after that summary boundary
      // need replay. Older servers retain the cursor-zero compatibility path.
      after: projection.kind === "snapshot" ? snapshot.cursor : 0,
      onEvent: (event) => {
        if (!this.#isCurrentGeneration(generation)) return;
        if (projection.kind === "snapshot") this.#receive(event);
        else this.#receiveAcrossSnapshotBoundary(event, snapshot.cursor);
      },
      onOpen: () => {
        if (!this.#isCurrentGeneration(generation)) return;
        if (skipFirstOpenRefresh) {
          skipFirstOpenRefresh = false;
          return;
        }
        void this.refreshProjection().catch(() => undefined);
      },
      onDiagnostic: this.#onDiagnostic,
    });
    if (!this.#isCurrentGeneration(generation)) {
      stream.close();
      return;
    }
    this.#stream = stream;
    stream.start();
  }

  async #serverProjectionBoundary(): Promise<ServerProjectionBoundary> {
    // Structural check keeps protocol-ingress unit fakes and pre-2.15 clients
    // on the same compatibility path as a real 404 response.
    if (typeof this.#client.serverProjectionSnapshot !== "function") {
      return { kind: "legacy-replay" };
    }
    try {
      return {
        kind: "snapshot",
        snapshot: await this.#client.serverProjectionSnapshot(),
      };
    } catch (cause) {
      if (cause instanceof ProtocolClientError && cause.status === 404) {
        return { kind: "legacy-replay" };
      }
      throw cause;
    }
  }

  /** Events at or before the atomic session-summary cursor are bootstrap
   * history. Fold only server projections that are absent from that snapshot;
   * suppress historical notifications, unknown-event notices, and lifecycle
   * metadata refreshes. Events after the boundary are genuinely live. */
  #receiveAcrossSnapshotBoundary(
    event: ProtocolIngressEvent,
    snapshotCursor: number,
  ): void {
    if (event.cursor > snapshotCursor) {
      this.#flushServerReplay(this.#generation);
      this.#receive(event);
      return;
    }
    if (event.kind !== "known" || !this.#serverReplay.push(event.envelope)) return;
    if (this.#serverReplayTimer !== undefined) return;
    const generation = this.#generation;
    this.#serverReplayTimer = this.#serverReplayScheduler.set(
      this.#serverReplayIdleFlushMs,
      () => {
        this.#serverReplayTimer = undefined;
        this.#flushServerReplay(generation);
      },
    );
  }

  #flushServerReplay(generation: number): void {
    if (this.#serverReplayTimer !== undefined) {
      this.#serverReplayScheduler.clear(this.#serverReplayTimer);
      this.#serverReplayTimer = undefined;
    }
    if (!this.#isCurrentGeneration(generation)) {
      this.#serverReplay.clear();
      return;
    }
    for (const envelope of this.#serverReplay.take()) {
      this.#store.applyServerEvent(envelope);
    }
  }

  #discardServerReplay(): void {
    if (this.#serverReplayTimer !== undefined) {
      this.#serverReplayScheduler.clear(this.#serverReplayTimer);
      this.#serverReplayTimer = undefined;
    }
    this.#serverReplay.clear();
  }

  async #refreshProjectionGeneration(generation: number): Promise<void> {
    const [info, boundary, projection] = await Promise.all([
      this.#client.serverInfo(),
      this.#sessionSummaryBoundary(),
      this.#serverProjectionBoundary(),
    ]);
    if (!this.#isCurrentGeneration(generation)) return;
    const metadataRevision = this.#metadataRevision;
    const [sessions, workspaces] = await Promise.all([
      this.#client.sessions(),
      this.#client.workspaces(),
    ]);
    if (!this.#isCurrentGeneration(generation)) return;
    const snapshot = materializeSessionSnapshot(boundary, sessions);
    this.#store.replaceServerInfo(info);
    if (metadataRevision === this.#metadataRevision) {
      this.#store.replaceSessionMetadata(sessions);
      this.#store.replaceWorkspaces(workspaces);
    }
    const acceptedCursor =
      this.#stream === undefined ? 0 : readSignal(this.#stream.cursor);
    if (snapshot.cursor >= acceptedCursor) {
      this.#store.replaceSessionSummaries(snapshot.summaries, snapshot.cursor);
      this.#onSessionSummaries(snapshot.summaries, snapshot.cursor);
    }
    if (
      projection.kind === "snapshot"
      && projection.snapshot.cursor >= acceptedCursor
    ) {
      this.#store.replaceServerProjection(
        projection.snapshot.cursor,
        projection.snapshot.value,
      );
    }
  }

  async #reconcileSessionActivityGeneration(generation: number): Promise<boolean> {
    const boundary = await this.#sessionSummaryBoundary();
    if (!this.#isCurrentGeneration(generation)) return false;
    const snapshot = boundary.kind === "atomic"
      ? boundary.snapshot
      : materializeSessionSnapshot(boundary, await this.#client.sessions());
    if (!this.#isCurrentGeneration(generation)) return false;
    const acceptedCursor =
      this.#stream === undefined ? 0 : readSignal(this.#stream.cursor);
    if (snapshot.cursor < acceptedCursor) return false;
    this.#store.replaceSessionSummaries(snapshot.summaries, snapshot.cursor);
    this.#onSessionSummaries(snapshot.summaries, snapshot.cursor);
    return true;
  }

  /** Early protocol 2.5 builds exposed session activity and cursor-bearing
   * settings snapshots but not the atomic session-summary route. Fence the
   * subsequent metadata reads with that same server-scope cursor so replay
   * still closes the snapshot/stream race. Every other summary failure is a
   * real bootstrap error and must remain visible to the caller. */
  async #sessionSummaryBoundary(): Promise<SessionSummaryBoundary> {
    try {
      return {
        kind: "atomic",
        snapshot: await this.#client.sessionSummaries(),
      };
    } catch (cause) {
      if (!(cause instanceof ProtocolClientError) || cause.status !== 404) {
        throw cause;
      }
      const fence = await this.#client.gitWorktreeSettingsSnapshot();
      return { kind: "metadata-fallback", cursor: fence.cursor };
    }
  }

  #receive(event: ProtocolIngressEvent): void {
    if (event.kind === "unknown") {
      this.#onUnknownEvent(event.type);
      return;
    }
    this.#onKnownEvent(event.envelope);
    if (event.envelope.type === "thread.created" || event.envelope.type === "thread.updated") {
      this.#scheduleThreadRefresh(event.envelope.session_id);
    }
    if (this.#store.applyServerEvent(event.envelope)) {
      this.#metadataRevision += 1;
      this.#scheduleMetadataRefresh();
    }
  }

  #scheduleThreadRefresh(sessionId: string): void {
    const active = this.#threadRefreshes.has(sessionId);
    this.#threadRefreshes.set(sessionId, active);
    if (!active) void this.#refreshThreads(sessionId, this.#generation);
  }

  async #refreshThreads(sessionId: string, generation: number): Promise<void> {
    while (this.#isCurrentGeneration(generation)) {
      this.#threadRefreshes.set(sessionId, false);
      try {
        const threads = await this.#client.threads(sessionId);
        if (!this.#isCurrentGeneration(generation)) return;
        // A second lifecycle event landed while this request was in flight.
        // Discard its stale response and fetch the final authoritative list.
        if (this.#threadRefreshes.get(sessionId)) continue;
        this.#store.replaceThreadsForSession(sessionId, threads);
      } catch {
        // Route ingress and the next lifecycle event both retry. Keep the
        // currently rendered tabs intact when a metadata read is transiently
        // unavailable.
        if (this.#threadRefreshes.get(sessionId)) continue;
      }
      this.#threadRefreshes.delete(sessionId);
      return;
    }
  }

  #scheduleMetadataRefresh(): void {
    this.#metadataRefreshPending = true;
    if (this.#metadataRefresh !== undefined) return;
    const generation = this.#generation;
    let refresh!: Promise<void>;
    refresh = this.#drainMetadataRefreshes(generation).finally(() => {
      if (this.#metadataRefresh !== refresh) return;
      this.#metadataRefresh = undefined;
      if (this.#started && this.#metadataRefreshPending) {
        this.#scheduleMetadataRefresh();
      }
    });
    this.#metadataRefresh = refresh;
  }

  async #drainMetadataRefreshes(generation: number): Promise<void> {
    while (this.#isCurrentGeneration(generation) && this.#metadataRefreshPending) {
      this.#metadataRefreshPending = false;
      try {
        const [sessions, workspaces] = await Promise.all([
          this.#client.sessions(),
          this.#client.workspaces(),
        ]);
        if (!this.#isCurrentGeneration(generation)) return;
        if (this.#metadataRefreshPending) continue;
        this.#store.replaceSessionMetadata(sessions);
        this.#store.replaceWorkspaces(workspaces);
      } catch {
        return;
      }
    }
  }

  #isCurrentGeneration(generation: number): boolean {
    return this.#started && generation === this.#generation;
  }

  #attachListeners(): void {
    if (this.#listenersAttached) return;
    this.#visibility?.addEventListener("visibilitychange", this.#foreground);
    this.#online?.addEventListener("online", this.#onlineAgain);
    this.#listenersAttached = true;
  }

  #detachListeners(): void {
    if (!this.#listenersAttached) return;
    this.#visibility?.removeEventListener("visibilitychange", this.#foreground);
    this.#online?.removeEventListener("online", this.#onlineAgain);
    this.#listenersAttached = false;
  }

  #recoverOrRefresh(): void {
    if (!this.#started) {
      void this.start().catch(() => undefined);
      return;
    }
    if (this.#bootstrap !== undefined || this.#stream === undefined) return;
    this.#stream.reconnectNow();
    void this.refreshProjection().catch(() => undefined);
  }

  readonly #foreground = (): void => {
    if (this.#visibility?.visibilityState !== "visible") return;
    this.#recoverOrRefresh();
  };

  readonly #onlineAgain = (): void => {
    this.#recoverOrRefresh();
  };
}

export const createBrowserProtocolIngress = (
  client: ProtocolClient,
  store: AppStore,
  options: {
    readonly onKnownEvent?: (event: ProtocolEventEnvelope) => void;
    readonly onSessionSummaries?: (
      summaries: readonly ProtocolSessionSummary[],
      cursor: number,
    ) => void;
  } = {},
): ProtocolIngress =>
  new ProtocolIngress(client, store, {
    ...(typeof document === "undefined" ? {} : { visibility: document }),
    ...(typeof window === "undefined" ? {} : { online: window }),
    ...options,
  });
