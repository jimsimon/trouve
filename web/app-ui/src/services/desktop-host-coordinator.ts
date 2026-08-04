import { createSignal, type ReadonlySignal } from "../state/reactivity.js";
import type {
  HostClient,
  HostLifecycleBatch,
  HostLifecycleState,
  HostPendingCloseRequest,
} from "./host-client.js";

export interface DesktopCloseActions {
  readonly cancel: () => Promise<void>;
  readonly quitNow: () => Promise<void>;
  readonly quitWhenIdle: () => Promise<void>;
}

export interface DesktopHostCoordinatorEnvironment {
  /** Presents the existing close confirmation UX. The host never owns this UI. */
  readonly onCloseRequested: (
    request: HostPendingCloseRequest,
    actions: DesktopCloseActions,
  ) => void;
  readonly onDiagnostic?: (error: unknown) => void;
}

export interface DesktopAppActivity {
  /** Derived from server-authoritative protocol/app state, never host state. */
  readonly idle: boolean;
  readonly workRunning: boolean;
  readonly preventSleepWhileRunning: boolean;
}

const INITIAL_LIFECYCLE_STATE: HostLifecycleState = Object.freeze({
  focused: true,
  visible: true,
  occluded: false,
  pendingClose: undefined,
});

/** Coordinates ephemeral desktop lifecycle mechanics with frontend-owned app
 * policy. Durable session/agent state remains exclusively in the protocol. */
export class DesktopHostCoordinator {
  readonly #host: Pick<
    HostClient,
    "watchLifecycle" | "resolveClose" | "setSleepInhibition"
  >;
  readonly #environment: DesktopHostCoordinatorEnvironment;
  readonly #lifecycle = createSignal<HostLifecycleState>(
    INITIAL_LIFECYCLE_STATE,
  );
  readonly lifecycle: ReadonlySignal<HostLifecycleState> = this.#lifecycle;

  #abort: AbortController | undefined;
  #idle = true;
  #announcedCloseRequest: number | undefined;
  #waitingForIdle: number | undefined;
  #idleQuitQueued = false;
  #closeTail: Promise<void> = Promise.resolve();
  #desiredSleepInhibition = false;
  #appliedSleepInhibition = false;
  #sleepDesiredVersion = 0;
  #failedSleepInhibition:
    | { readonly value: boolean; readonly version: number }
    | undefined;
  #sleepSyncRunning = false;

  constructor(
    host: Pick<
      HostClient,
      "watchLifecycle" | "resolveClose" | "setSleepInhibition"
    >,
    environment: DesktopHostCoordinatorEnvironment,
  ) {
    this.#host = host;
    this.#environment = environment;
  }

  start(): void {
    if (this.#abort !== undefined) return;
    const abort = new AbortController();
    this.#abort = abort;
    void this.#host
      .watchLifecycle((batch) => this.#receive(batch), { signal: abort.signal })
      .catch((error: unknown) => {
        if (!abort.signal.aborted) this.#diagnostic(error);
      });
  }

  stop(): void {
    this.#abort?.abort();
    this.#abort = undefined;
    this.#announcedCloseRequest = undefined;
    this.#waitingForIdle = undefined;
    this.#setDesiredSleepInhibition(false);
    this.#synchronizeSleepInhibition();
  }

  /** Call whenever protocol-backed running/idle state or the general
   * preference changes. This is the only source of quit-idle and sleep policy. */
  updateActivity(activity: DesktopAppActivity): void {
    this.#idle = activity.idle;
    this.#setDesiredSleepInhibition(
      activity.workRunning && activity.preventSleepWhileRunning,
    );
    this.#synchronizeSleepInhibition();
    this.#quitWhenIdleIfReady();
  }

  #receive(batch: HostLifecycleBatch): void {
    this.#lifecycle.set(batch.state);
    const pending = batch.state.pendingClose;
    if (pending === undefined) {
      this.#announcedCloseRequest = undefined;
      this.#waitingForIdle = undefined;
      return;
    }
    if (pending.waitingForIdle) {
      this.#announcedCloseRequest = pending.requestId;
      this.#waitingForIdle = pending.requestId;
      this.#quitWhenIdleIfReady();
      return;
    }
    this.#waitingForIdle = undefined;
    if (this.#announcedCloseRequest === pending.requestId) return;
    this.#announcedCloseRequest = pending.requestId;
    try {
      this.#environment.onCloseRequested(
        pending,
        Object.freeze({
          cancel: () => this.#resolveClose(pending.requestId, "cancel"),
          quitNow: () => this.#resolveClose(pending.requestId, "quit_now"),
          quitWhenIdle: () =>
            this.#resolveClose(pending.requestId, "quit_when_idle"),
        }),
      );
    } catch (error) {
      this.#diagnostic(error);
    }
  }

  #resolveClose(
    requestId: number,
    decision: "cancel" | "quit_now" | "quit_when_idle",
  ): Promise<void> {
    const operation = this.#closeTail.then(async () => {
      await this.#host.resolveClose(requestId, decision);
      if (decision === "quit_when_idle") {
        this.#waitingForIdle = requestId;
        if (this.#idle) {
          await this.#host.resolveClose(requestId, "quit_now");
          this.#waitingForIdle = undefined;
        }
      } else {
        this.#waitingForIdle = undefined;
        this.#announcedCloseRequest = undefined;
      }
    });
    this.#closeTail = operation.catch((error: unknown) => {
      this.#diagnostic(error);
    });
    return operation;
  }

  #quitWhenIdleIfReady(): void {
    const requestId = this.#waitingForIdle;
    if (!this.#idle || requestId === undefined || this.#idleQuitQueued) return;
    this.#idleQuitQueued = true;
    const operation = this.#closeTail.then(async () => {
      if (this.#idle && this.#waitingForIdle === requestId) {
        await this.#host.resolveClose(requestId, "quit_now");
        this.#waitingForIdle = undefined;
        this.#announcedCloseRequest = undefined;
      }
    });
    this.#closeTail = operation
      .catch((error: unknown) => this.#diagnostic(error))
      .finally(() => {
        this.#idleQuitQueued = false;
        this.#quitWhenIdleIfReady();
      });
  }

  #synchronizeSleepInhibition(): void {
    if (
      this.#sleepSyncRunning ||
      this.#appliedSleepInhibition === this.#desiredSleepInhibition ||
      (this.#failedSleepInhibition?.value === this.#desiredSleepInhibition &&
        this.#failedSleepInhibition.version === this.#sleepDesiredVersion)
    ) {
      return;
    }
    this.#sleepSyncRunning = true;
    void (async () => {
      while (this.#appliedSleepInhibition !== this.#desiredSleepInhibition) {
        const next = this.#desiredSleepInhibition;
        const version = this.#sleepDesiredVersion;
        try {
          await this.#host.setSleepInhibition(next);
          this.#appliedSleepInhibition = next;
          this.#failedSleepInhibition = undefined;
        } catch (error) {
          this.#failedSleepInhibition = { value: next, version };
          this.#diagnostic(error);
          break;
        }
      }
    })().finally(() => {
      this.#sleepSyncRunning = false;
      // A transition that happened while the host request was in flight must
      // not be lost. A failed value remains suppressed until the desired
      // state changes, matching the native frontend's idle-to-active retry
      // policy without spinning on an unavailable host.
      this.#synchronizeSleepInhibition();
    });
  }

  #setDesiredSleepInhibition(next: boolean): void {
    if (next === this.#desiredSleepInhibition) return;
    this.#desiredSleepInhibition = next;
    this.#sleepDesiredVersion += 1;
    this.#failedSleepInhibition = undefined;
  }

  #diagnostic(error: unknown): void {
    try {
      this.#environment.onDiagnostic?.(error);
    } catch {
      // Diagnostics must never interrupt lifecycle or close handling.
    }
  }
}
