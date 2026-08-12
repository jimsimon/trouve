import type { AppStore } from "../state/app-store.js";
import { createSignal, type ReadonlySignal } from "../state/reactivity.js";
import type { CursorEventStream, SafeStreamDiagnostic } from "./cursor-event-stream.js";
import type {
  ProtocolCursorSnapshot,
  ProtocolEventEnvelope,
  ProtocolIngressEvent,
  ProtocolThread,
  ProtocolThreadStatus,
  ProtocolThreadViewSnapshot,
} from "./protocol-client.js";

type ThreadStream = CursorEventStream<ProtocolIngressEvent>;

export interface ThreadProtocol {
  threads(sessionId: string): Promise<readonly ProtocolThread[]>;
  threadStatuses?(sessionId: string): Promise<readonly ProtocolThreadStatus[]>;
  threadView(
    threadId: string,
    before?: number,
  ): Promise<ProtocolCursorSnapshot<ProtocolThreadViewSnapshot>>;
  threadEvents(
    threadId: string,
    options: {
      readonly after: number;
      readonly onEvent: (event: ProtocolIngressEvent) => void;
      readonly onOpen?: () => void;
      readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
    },
  ): Promise<ThreadStream>;
}

export type ThreadIngressState = "idle" | "loading" | "open" | "error";

interface VisibilitySource {
  readonly visibilityState: DocumentVisibilityState;
  addEventListener(type: "visibilitychange", listener: () => void): void;
  removeEventListener(type: "visibilitychange", listener: () => void): void;
}

interface OnlineSource {
  addEventListener(type: "online", listener: () => void): void;
  removeEventListener(type: "online", listener: () => void): void;
}

export interface ThreadReplayScheduler {
  set(delayMs: number, callback: () => void): unknown;
  clear(handle: unknown): void;
}

const browserReplayScheduler: ThreadReplayScheduler = {
  set: (delayMs, callback) => globalThis.setTimeout(callback, delayMs),
  clear: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

const PERSISTED_REPLAY_AGE_MS = 2_000;
const REPLAY_IDLE_FLUSH_MS = 25;

/** Coalesce old reconnect/startup history without delaying genuinely live
 * events. The event order stays exact and a live envelope flushes any replay
 * prefix in the same store transaction. */
export class ThreadReplayBatcher {
  readonly #apply: (events: readonly ProtocolEventEnvelope[]) => void;
  readonly #now: () => number;
  readonly #scheduler: ThreadReplayScheduler;
  readonly #idleFlushMs: number;
  #pending: ProtocolEventEnvelope[] = [];
  #timer: unknown;
  #disposed = false;

  constructor(
    apply: (events: readonly ProtocolEventEnvelope[]) => void,
    options: {
      readonly now?: () => number;
      readonly scheduler?: ThreadReplayScheduler;
      readonly idleFlushMs?: number;
    } = {},
  ) {
    this.#apply = apply;
    this.#now = options.now ?? Date.now;
    this.#scheduler = options.scheduler ?? browserReplayScheduler;
    this.#idleFlushMs = options.idleFlushMs ?? REPLAY_IDLE_FLUSH_MS;
  }

  receive(envelope: ProtocolEventEnvelope): void {
    if (this.#disposed) return;
    const timestamp = Date.parse(envelope.ts);
    const persisted = Number.isFinite(timestamp)
      && this.#now() - timestamp > PERSISTED_REPLAY_AGE_MS;
    if (!persisted) {
      this.#pending.push(envelope);
      this.flush();
      return;
    }
    this.#pending.push(envelope);
    if (this.#timer !== undefined) return;
    this.#timer = this.#scheduler.set(this.#idleFlushMs, () => {
      this.#timer = undefined;
      this.#flushPending();
    });
  }

  flush(): void {
    if (this.#disposed) return;
    if (this.#timer !== undefined) {
      this.#scheduler.clear(this.#timer);
      this.#timer = undefined;
    }
    this.#flushPending();
  }

  dispose(): void {
    this.#disposed = true;
    if (this.#timer !== undefined) this.#scheduler.clear(this.#timer);
    this.#timer = undefined;
    this.#pending = [];
  }

  #flushPending(): void {
    if (this.#disposed || this.#pending.length === 0) return;
    const pending = this.#pending;
    this.#pending = [];
    this.#apply(pending);
  }
}

/** Owns exactly one active thread stream. Revisiting a thread resumes after
 * its retained view-model cursor; switching threads closes the old stream. */
export class ThreadIngress {
  readonly #client: ThreadProtocol;
  readonly #store: AppStore;
  readonly #visibility: VisibilitySource | undefined;
  readonly #online: OnlineSource | undefined;
  readonly #onDiagnostic: (diagnostic: SafeStreamDiagnostic) => void;
  readonly #onUnknownEvent: (type: string) => void;
  readonly #now: () => number;
  readonly #replayScheduler: ThreadReplayScheduler;
  readonly #replayIdleFlushMs: number;
  readonly #state = createSignal<ThreadIngressState>("idle");

  readonly state: ReadonlySignal<ThreadIngressState> = this.#state;

  #stream: ThreadStream | undefined;
  #replayBatcher: ThreadReplayBatcher | undefined;
  #generation = 0;
  #listenersAttached = false;

  constructor(
    client: ThreadProtocol,
    store: AppStore,
    options: {
      readonly visibility?: VisibilitySource;
      readonly online?: OnlineSource;
      readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
      readonly onUnknownEvent?: (type: string) => void;
      readonly now?: () => number;
      readonly replayScheduler?: ThreadReplayScheduler;
      readonly replayIdleFlushMs?: number;
    } = {},
  ) {
    this.#client = client;
    this.#store = store;
    this.#visibility = options.visibility;
    this.#online = options.online;
    this.#onDiagnostic = options.onDiagnostic ?? (() => undefined);
    this.#onUnknownEvent = options.onUnknownEvent ?? (() => undefined);
    this.#now = options.now ?? Date.now;
    this.#replayScheduler = options.replayScheduler ?? browserReplayScheduler;
    this.#replayIdleFlushMs = options.replayIdleFlushMs ?? REPLAY_IDLE_FLUSH_MS;
  }

  async openSession(
    sessionId: string,
    requestedThreadId?: string,
    closedThreadIds: readonly string[] = [],
  ): Promise<string | undefined> {
    const generation = ++this.#generation;
    this.#state.set("loading");
    try {
      const [threads, statuses] = await Promise.all([
        this.#client.threads(sessionId),
        this.#client.threadStatuses?.(sessionId).catch(() => undefined)
          ?? Promise.resolve(undefined),
      ]);
      if (generation !== this.#generation) return undefined;
      this.#store.replaceThreadsForSession(sessionId, threads);
      if (statuses !== undefined) {
        this.#store.replaceThreadStatusesForSession(sessionId, statuses);
      }
      const closed = new Set(closedThreadIds);
      let latestOpen: typeof threads[number] | undefined;
      for (let index = threads.length - 1; index >= 0; index -= 1) {
        const candidate = threads[index];
        if (candidate !== undefined && !closed.has(candidate.id)) {
          latestOpen = candidate;
          break;
        }
      }
      const selected = requestedThreadId === undefined
        ? latestOpen
        : threads.find((thread) => thread.id === requestedThreadId) ?? latestOpen;
      if (selected === undefined) {
        this.#closeStream();
        this.#state.set("open");
        return undefined;
      }
      await this.#openThread(selected.id, generation);
      return generation === this.#generation ? selected.id : undefined;
    } catch (error) {
      if (generation === this.#generation) this.#state.set("error");
      throw error;
    }
  }

  close(): void {
    this.#generation += 1;
    this.#closeStream();
    this.#state.set("idle");
  }

  async #openThread(threadId: string, generation: number): Promise<void> {
    // `openSession` advances the generation before resolving the thread list.
    // Reusing a same-thread stream here would leave its callbacks captured to
    // the previous generation, causing every subsequent event to be dropped.
    // Reconnect from the retained cursor so the active stream and generation
    // always agree.
    this.#closeStream();
    const snapshot = await this.#client.threadView(threadId);
    if (generation !== this.#generation) return;
    this.#store.replaceThreadViewSnapshot(
      threadId,
      snapshot.cursor,
      snapshot.value,
    );
    const view = this.#store.threadView(threadId);
    const replayBatcher = new ThreadReplayBatcher(
      (events) => {
        if (generation !== this.#generation) return;
        this.#store.applyThreadEvents(threadId, events);
      },
      {
        now: this.#now,
        scheduler: this.#replayScheduler,
        idleFlushMs: this.#replayIdleFlushMs,
      },
    );
    const stream = await this.#client.threadEvents(threadId, {
      after: view.cursor,
      onEvent: (event) => {
        if (generation !== this.#generation) return;
        if (event.kind === "unknown") {
          this.#onUnknownEvent(event.type);
          return;
        }
        replayBatcher.receive(event.envelope);
      },
      onOpen: () => {
        if (generation === this.#generation) this.#state.set("open");
      },
      onDiagnostic: this.#onDiagnostic,
    });
    if (generation !== this.#generation) {
      replayBatcher.dispose();
      stream.close();
      return;
    }
    this.#stream = stream;
    this.#replayBatcher = replayBatcher;
    this.#attachListeners();
    stream.start();
  }

  #closeStream(): void {
    this.#detachListeners();
    this.#replayBatcher?.dispose();
    this.#replayBatcher = undefined;
    this.#stream?.close();
    this.#stream = undefined;
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

  readonly #foreground = (): void => {
    if (this.#visibility?.visibilityState !== "visible") return;
    this.#stream?.reconnectNow();
  };

  readonly #onlineAgain = (): void => {
    this.#stream?.reconnectNow();
  };
}

export const createBrowserThreadIngress = (
  client: ThreadProtocol,
  store: AppStore,
  options: {
    readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
    readonly onUnknownEvent?: (type: string) => void;
  } = {},
): ThreadIngress => new ThreadIngress(client, store, {
  ...(typeof document === "undefined" ? {} : { visibility: document }),
  ...(typeof window === "undefined" ? {} : { online: window }),
  ...options,
});
