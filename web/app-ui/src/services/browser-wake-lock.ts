export interface WakeLockSentinelLike {
  readonly released: boolean;
  release(): Promise<void>;
  addEventListener(type: "release", listener: () => void): void;
}

export interface WakeLockManagerLike {
  request(type: "screen"): Promise<WakeLockSentinelLike>;
}

export interface WakeLockNavigatorLike {
  readonly wakeLock?: WakeLockManagerLike;
}

export interface WakeLockDocumentLike {
  readonly visibilityState: DocumentVisibilityState;
  addEventListener(type: "visibilitychange", listener: () => void): void;
  removeEventListener(type: "visibilitychange", listener: () => void): void;
}

export const browserWakeLockCapability = (
  navigatorLike: WakeLockNavigatorLike | undefined = globalThis.navigator,
): boolean => typeof navigatorLike?.wakeLock?.request === "function";

/** Owns the PWA's ephemeral screen wake lock. The desired state comes from
 * server-authoritative session activity plus the persisted user preference;
 * the browser is allowed to revoke the actual sentinel at any time. */
export class BrowserWakeLockCoordinator {
  readonly #navigator: WakeLockNavigatorLike;
  readonly #document: WakeLockDocumentLike;
  #desired = false;
  #started = false;
  #generation = 0;
  #requestPending = false;
  #sentinel: WakeLockSentinelLike | undefined;

  constructor(
    navigatorLike: WakeLockNavigatorLike = globalThis.navigator,
    documentLike: WakeLockDocumentLike = globalThis.document,
  ) {
    this.#navigator = navigatorLike;
    this.#document = documentLike;
  }

  get held(): boolean {
    return this.#sentinel !== undefined && !this.#sentinel.released;
  }

  start(): void {
    if (this.#started) return;
    this.#started = true;
    this.#document.addEventListener("visibilitychange", this.#visibilityChanged);
    void this.#reconcile();
  }

  stop(): void {
    if (!this.#started) return;
    this.#started = false;
    this.#generation += 1;
    this.#document.removeEventListener("visibilitychange", this.#visibilityChanged);
    const sentinel = this.#sentinel;
    this.#sentinel = undefined;
    if (sentinel !== undefined && !sentinel.released) {
      void sentinel.release().catch(() => undefined);
    }
  }

  setDesired(desired: boolean): void {
    if (this.#desired === desired) return;
    this.#desired = desired;
    this.#generation += 1;
    void this.#reconcile();
  }

  readonly #visibilityChanged = (): void => {
    this.#generation += 1;
    void this.#reconcile();
  };

  async #reconcile(): Promise<void> {
    const shouldHold = this.#started && this.#desired &&
      this.#document.visibilityState === "visible";
    if (!shouldHold) {
      const sentinel = this.#sentinel;
      this.#sentinel = undefined;
      if (sentinel !== undefined && !sentinel.released) {
        await sentinel.release().catch(() => undefined);
      }
      return;
    }
    if (this.held || this.#requestPending || !browserWakeLockCapability(this.#navigator)) {
      return;
    }

    const generation = this.#generation;
    this.#requestPending = true;
    let sentinel: WakeLockSentinelLike | undefined;
    try {
      sentinel = await this.#navigator.wakeLock!.request("screen");
    } catch {
      // Permission, policy, battery, and platform failures are expected. A
      // later activity or visibility transition gets another chance.
    } finally {
      this.#requestPending = false;
    }
    if (sentinel === undefined) return;
    if (
      generation !== this.#generation ||
      !this.#started ||
      !this.#desired ||
      this.#document.visibilityState !== "visible"
    ) {
      await sentinel.release().catch(() => undefined);
      return;
    }
    this.#sentinel = sentinel;
    sentinel.addEventListener("release", () => {
      if (this.#sentinel !== sentinel) return;
      this.#sentinel = undefined;
    });
  }
}
