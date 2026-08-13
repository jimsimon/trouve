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
  /** Reconciles protocol activity immediately before an automatic quit.
   * Reject for unavailable/unknown state and return false when work is active. */
  readonly confirmAutomaticClose: () => Promise<boolean>;
  /** Presents the existing close confirmation UX. The host never owns this UI. */
  readonly onCloseRequested: (
    request: HostPendingCloseRequest,
    actions: DesktopCloseActions,
  ) => void;
  readonly onDiagnostic?: (error: unknown) => void;
  readonly closeRetryScheduler?: {
    readonly set: (delayMs: number, callback: () => void) => unknown;
    readonly clear: (handle: unknown) => void;
  };
}

export interface DesktopAppActivity {
  /** Derived from server-authoritative protocol/app state, never host state. */
  readonly authoritative: boolean;
  readonly idle: boolean;
  readonly workRunning: boolean;
  readonly preventSleepWhileRunning: boolean;
}

export const canAutomaticallyCloseDesktop = (
  activity: Pick<DesktopAppActivity, "authoritative" | "idle">,
): boolean => activity.authoritative && activity.idle;

const INITIAL_LIFECYCLE_STATE: HostLifecycleState = Object.freeze({
  focused: true,
  visible: true,
  occluded: false,
  pendingClose: undefined,
});

const CLOSE_RETRY_BASE_MS = 1_000;
const CLOSE_RETRY_MAX_MS = 10_000;
const DEFAULT_CLOSE_RETRY_SCHEDULER = {
  set: (delayMs: number, callback: () => void): unknown =>
    globalThis.setTimeout(callback, delayMs),
  clear: (handle: unknown): void =>
    globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

type DesktopCoordinatorHost = Pick<
  HostClient,
  "watchLifecycle" | "resolveClose" | "setSleepInhibition"
> & Partial<Pick<HostClient, "acknowledgeClose">>;

/** Coordinates ephemeral desktop lifecycle mechanics with frontend-owned app
 * policy. Durable session/agent state remains exclusively in the protocol. */
export class DesktopHostCoordinator {
  readonly #host: DesktopCoordinatorHost;
  readonly #environment: DesktopHostCoordinatorEnvironment;
  readonly #closeRetryScheduler: NonNullable<
    DesktopHostCoordinatorEnvironment["closeRetryScheduler"]
  >;
  readonly #lifecycle = createSignal<HostLifecycleState>(
    INITIAL_LIFECYCLE_STATE,
  );
  readonly lifecycle: ReadonlySignal<HostLifecycleState> = this.#lifecycle;

  #abort: AbortController | undefined;
  #activityAuthoritative = false;
  #idle = false;
  #activityVersion = 0;
  #announcedCloseRequest: number | undefined;
  #waitingForIdle: number | undefined;
  #idleQuitQueued = false;
  #failedIdleQuitRequest: number | undefined;
  #idleQuitRetry: unknown;
  #idleQuitRetryAttempt = 0;
  #closeTail: Promise<void> = Promise.resolve();
  #closeAcknowledgementQueued: number | undefined;
  #acknowledgedCloseRequest: number | undefined;
  #closeAcknowledgementRetry: unknown;
  #closeAcknowledgementRetryAttempt = 0;
  #closeAcknowledgementGeneration = 0;
  #desiredSleepInhibition = false;
  #appliedSleepInhibition = false;
  #sleepDesiredVersion = 0;
  #failedSleepInhibition:
    | { readonly value: boolean; readonly version: number }
    | undefined;
  #sleepSyncRunning = false;

  constructor(
    host: DesktopCoordinatorHost,
    environment: DesktopHostCoordinatorEnvironment,
  ) {
    this.#host = host;
    this.#environment = environment;
    this.#closeRetryScheduler = environment.closeRetryScheduler
      ?? DEFAULT_CLOSE_RETRY_SCHEDULER;
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
    this.#activityAuthoritative = false;
    this.#idle = false;
    this.#clearCloseAcknowledgement();
    this.#clearIdleQuitRetry();
    this.#setDesiredSleepInhibition(false);
    this.#synchronizeSleepInhibition();
  }

  /** Call whenever protocol-backed running/idle state or the general
   * preference changes. This is the only source of quit-idle and sleep policy. */
  updateActivity(activity: DesktopAppActivity): void {
    if (
      this.#activityAuthoritative !== activity.authoritative ||
      this.#idle !== activity.idle
    ) {
      this.#failedIdleQuitRequest = undefined;
      this.#activityVersion += 1;
      this.#clearIdleQuitRetry();
    }
    this.#activityAuthoritative = activity.authoritative;
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
      this.#failedIdleQuitRequest = undefined;
      this.#clearIdleQuitRetry();
      this.#clearCloseAcknowledgement();
      return;
    }
    if (pending.waitingForIdle) {
      if (this.#waitingForIdle !== pending.requestId) {
        this.#failedIdleQuitRequest = undefined;
        this.#clearIdleQuitRetry();
      }
      this.#announcedCloseRequest = pending.requestId;
      this.#waitingForIdle = pending.requestId;
      this.#quitWhenIdleIfReady();
      return;
    }
    if (this.#announcedCloseRequest === pending.requestId) return;
    this.#waitingForIdle = undefined;
    this.#failedIdleQuitRequest = undefined;
    this.#announcedCloseRequest = pending.requestId;
    this.#acknowledgeClose(pending.requestId);
    // Cached protocol state is only a candidate. The SSE stream can be
    // reconnecting while the last projection still looks idle, so every
    // automatic quit performs a close-time activity reconciliation first.
    if (canAutomaticallyCloseDesktop({
      authoritative: this.#activityAuthoritative,
      idle: this.#idle,
    })) {
      void this.#confirmAndResolveAutomaticClose(pending);
      return;
    }
    this.#presentCloseRequest(pending);
  }

  async #confirmAndResolveAutomaticClose(
    pending: HostPendingCloseRequest,
  ): Promise<void> {
    let confirmed = false;
    try {
      confirmed = await this.#environment.confirmAutomaticClose();
    } catch (error) {
      this.#diagnostic(error);
    }
    if (this.#abort === undefined) return;
    const current = this.#lifecycle.get().pendingClose;
    if (
      current?.requestId !== pending.requestId
      || current.waitingForIdle
    ) return;
    if (
      confirmed
      && canAutomaticallyCloseDesktop({
        authoritative: this.#activityAuthoritative,
        idle: this.#idle,
      })
    ) {
      try {
        await this.#resolveClose(pending.requestId, "quit_now");
      } catch {
        const latest = this.#lifecycle.get().pendingClose;
        if (latest?.requestId === pending.requestId && !latest.waitingForIdle) {
          this.#presentCloseRequest(pending);
        }
      }
      return;
    }
    this.#presentCloseRequest(pending);
  }

  #presentCloseRequest(pending: HostPendingCloseRequest): void {
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
      this.#announcedCloseRequest = undefined;
      this.#diagnostic(error);
    }
  }

  #resolveClose(
    requestId: number,
    decision: "cancel" | "quit_now" | "quit_when_idle",
  ): Promise<void> {
    const operation = this.#closeTail.then(async () => {
      await this.#host.resolveClose(requestId, decision);
      this.#clearCloseAcknowledgement();
      if (decision === "quit_when_idle") {
        this.#waitingForIdle = requestId;
        this.#quitWhenIdleIfReady();
      } else {
        this.#waitingForIdle = undefined;
        this.#announcedCloseRequest = undefined;
        this.#failedIdleQuitRequest = undefined;
        this.#clearIdleQuitRetry();
      }
    });
    this.#closeTail = operation.catch((error: unknown) => {
      this.#diagnostic(error);
    });
    return operation;
  }

  #acknowledgeClose(requestId: number): void {
    if (
      this.#abort === undefined
      ||
      this.#host.acknowledgeClose === undefined
      || this.#acknowledgedCloseRequest === requestId
      || this.#closeAcknowledgementQueued === requestId
    ) return;
    if (
      this.#acknowledgedCloseRequest !== undefined
      || this.#closeAcknowledgementQueued !== undefined
    ) this.#clearCloseAcknowledgement();
    this.#closeAcknowledgementQueued = requestId;
    const generation = this.#closeAcknowledgementGeneration;
    void this.#host.acknowledgeClose(requestId)
      .then(() => {
        if (
          this.#abort !== undefined
          && this.#closeAcknowledgementGeneration === generation
          && this.#lifecycle.get().pendingClose?.requestId === requestId
        ) {
          this.#acknowledgedCloseRequest = requestId;
          this.#clearCloseAcknowledgementRetry();
        }
      })
      .catch((error: unknown) => {
        if (
          this.#abort !== undefined
          && this.#closeAcknowledgementGeneration === generation
          && this.#lifecycle.get().pendingClose?.requestId === requestId
        ) {
          this.#diagnostic(error);
          this.#scheduleCloseAcknowledgementRetry(requestId);
        }
      })
      .finally(() => {
        if (
          this.#closeAcknowledgementGeneration === generation
          && this.#closeAcknowledgementQueued === requestId
        ) {
          this.#closeAcknowledgementQueued = undefined;
        }
      });
  }

  #scheduleCloseAcknowledgementRetry(requestId: number): void {
    if (this.#abort === undefined || this.#closeAcknowledgementRetry !== undefined) return;
    const generation = this.#closeAcknowledgementGeneration;
    const delay = Math.min(
      CLOSE_RETRY_BASE_MS * 2 ** this.#closeAcknowledgementRetryAttempt,
      CLOSE_RETRY_MAX_MS,
    );
    this.#closeAcknowledgementRetryAttempt += 1;
    this.#closeAcknowledgementRetry = this.#closeRetryScheduler.set(delay, () => {
      this.#closeAcknowledgementRetry = undefined;
      if (
        this.#abort === undefined
        || this.#closeAcknowledgementGeneration !== generation
        || this.#lifecycle.get().pendingClose?.requestId !== requestId
      ) return;
      this.#acknowledgeClose(requestId);
    });
  }

  #clearCloseAcknowledgementRetry(): void {
    if (this.#closeAcknowledgementRetry !== undefined) {
      this.#closeRetryScheduler.clear(this.#closeAcknowledgementRetry);
      this.#closeAcknowledgementRetry = undefined;
    }
    this.#closeAcknowledgementRetryAttempt = 0;
  }

  #clearCloseAcknowledgement(): void {
    this.#closeAcknowledgementGeneration += 1;
    this.#clearCloseAcknowledgementRetry();
    this.#closeAcknowledgementQueued = undefined;
    this.#acknowledgedCloseRequest = undefined;
  }

  #quitWhenIdleIfReady(): void {
    const requestId = this.#waitingForIdle;
    if (
      !this.#activityAuthoritative ||
      !this.#idle ||
      requestId === undefined ||
      this.#idleQuitQueued ||
      this.#failedIdleQuitRequest === requestId
    ) return;
    this.#idleQuitQueued = true;
    const activityVersion = this.#activityVersion;
    const operation = this.#closeTail.then(async () => {
      if (!canAutomaticallyCloseDesktop({
        authoritative: this.#activityAuthoritative,
        idle: this.#idle,
      }) || this.#waitingForIdle !== requestId) return;
      const confirmed = await this.#environment.confirmAutomaticClose();
      if (!confirmed) {
        if (canAutomaticallyCloseDesktop({
          authoritative: this.#activityAuthoritative,
          idle: this.#idle,
        }) && this.#waitingForIdle === requestId) {
          throw new Error("automatic close activity was not confirmed");
        }
        return;
      }
      if (!canAutomaticallyCloseDesktop({
        authoritative: this.#activityAuthoritative,
        idle: this.#idle,
      }) || this.#waitingForIdle !== requestId) return;
      await this.#host.resolveClose(requestId, "quit_now");
      this.#waitingForIdle = undefined;
      this.#announcedCloseRequest = undefined;
      this.#failedIdleQuitRequest = undefined;
      this.#clearIdleQuitRetry();
    });
    this.#closeTail = operation
      .catch((error: unknown) => {
        this.#failedIdleQuitRequest = requestId;
        this.#diagnostic(error);
        this.#scheduleIdleQuitRetry(requestId);
      })
      .finally(() => {
        this.#idleQuitQueued = false;
        if (this.#activityVersion !== activityVersion) {
          this.#quitWhenIdleIfReady();
        }
      });
  }

  #scheduleIdleQuitRetry(requestId: number): void {
    if (
      this.#idleQuitRetry !== undefined
      || this.#waitingForIdle !== requestId
    ) return;
    const delay = Math.min(
      CLOSE_RETRY_BASE_MS * 2 ** this.#idleQuitRetryAttempt,
      CLOSE_RETRY_MAX_MS,
    );
    this.#idleQuitRetryAttempt += 1;
    this.#idleQuitRetry = this.#closeRetryScheduler.set(delay, () => {
      this.#idleQuitRetry = undefined;
      if (this.#waitingForIdle !== requestId) return;
      this.#failedIdleQuitRequest = undefined;
      this.#quitWhenIdleIfReady();
    });
  }

  #clearIdleQuitRetry(): void {
    if (this.#idleQuitRetry !== undefined) {
      this.#closeRetryScheduler.clear(this.#idleQuitRetry);
      this.#idleQuitRetry = undefined;
    }
    this.#idleQuitRetryAttempt = 0;
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
