import type {
  ProtocolClient,
  ProtocolModelInfo,
} from "./protocol-client.js";
import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

export type ModelCatalogFreshness = "if-stale" | "force";

const DEFAULT_LIVE_TTL_MS = 300_000;

/** App-wide stale-while-revalidate model catalog.
 *
 * The static endpoint is intentionally first-paint safe. Live provider and
 * vendor-CLI discovery follows in one coalesced request and replaces the
 * signal when it resolves, so Cursor can enrich every picker without holding
 * any setup or composer control behind ACP startup.
 */
export class ModelCatalogController {
  readonly #protocol: Pick<ProtocolClient, "models" | "refreshModels">;
  readonly #now: () => number;
  readonly #liveTtlMs: number;
  readonly #current = createSignal<readonly ProtocolModelInfo[]>(
    Object.freeze([]),
  );
  readonly current: ReadonlySignal<readonly ProtocolModelInfo[]> = this.#current;
  readonly #static = createSignal<readonly ProtocolModelInfo[]>(
    Object.freeze([]),
  );
  readonly staticCurrent: ReadonlySignal<readonly ProtocolModelInfo[]> = this.#static;
  readonly #live = createSignal<readonly ProtocolModelInfo[]>(
    Object.freeze([]),
  );
  readonly #refreshing = createSignal(false);
  readonly refreshing: ReadonlySignal<boolean> = this.#refreshing;

  #staticPending: Promise<readonly ProtocolModelInfo[]> | undefined;
  #livePending: Promise<readonly ProtocolModelInfo[]> | undefined;
  #staticLoaded = false;
  #liveLoaded = false;
  #lastLiveResolvedAt: number | undefined;
  #generation = 0;

  constructor(
    protocol: Pick<ProtocolClient, "models" | "refreshModels">,
    options: { readonly now?: () => number; readonly liveTtlMs?: number } = {},
  ) {
    this.#protocol = protocol;
    this.#now = options.now ?? (() => Date.now());
    this.#liveTtlMs = options.liveTtlMs ?? DEFAULT_LIVE_TTL_MS;
  }

  refresh(
    freshness: ModelCatalogFreshness = "if-stale",
  ): Promise<readonly ProtocolModelInfo[]> {
    const current = this.#current.get();
    if (freshness === "if-stale" && current.length > 0) {
      void this.#refreshLive(freshness).catch(() => undefined);
      return Promise.resolve(current);
    }
    const immediate = this.#loadStatic();
    void immediate
      .then(() => this.#refreshLive(freshness))
      .catch(() => undefined);
    return immediate;
  }

  /** Authoritative offline-safe metadata used to resolve configured defaults. */
  staticModels(): Promise<readonly ProtocolModelInfo[]> {
    if (this.#staticPending !== undefined) {
      return this.#staticLoaded
        ? this.#staticPending.catch(() => this.#static.get())
        : this.#staticPending;
    }
    const current = this.#static.get();
    return this.#staticLoaded ? Promise.resolve(current) : this.#loadStatic();
  }

  #loadStatic(): Promise<readonly ProtocolModelInfo[]> {
    if (this.#staticPending !== undefined) return this.#staticPending;
    const promise = this.#protocol.models().then((models) => {
      const snapshot = Object.freeze([...models]);
      this.#staticLoaded = true;
      this.#static.set(snapshot);
      this.#current.set(this.#liveLoaded ? this.#live.get() : snapshot);
      return snapshot;
    }).finally(() => {
      if (this.#staticPending === promise) this.#staticPending = undefined;
    });
    this.#staticPending = promise;
    return promise;
  }

  #refreshLive(
    freshness: ModelCatalogFreshness,
  ): Promise<readonly ProtocolModelInfo[]> {
    if (this.#livePending !== undefined) return this.#livePending;
    const now = this.#now();
    const fresh = this.#lastLiveResolvedAt !== undefined
      && Math.max(0, now - this.#lastLiveResolvedAt) < this.#liveTtlMs;
    if (freshness === "if-stale" && fresh) {
      return Promise.resolve(this.#current.get());
    }

    const generation = ++this.#generation;
    this.#refreshing.set(true);
    const promise = this.#protocol.refreshModels().then(
      (models) => {
        if (generation !== this.#generation) return this.#current.get();
        const snapshot = Object.freeze([...models]);
        this.#lastLiveResolvedAt = this.#now();
        this.#liveLoaded = true;
        this.#live.set(snapshot);
        this.#current.set(snapshot);
        return snapshot;
      },
      (error: unknown) => {
        if (generation !== this.#generation) return this.#current.get();
        throw error;
      },
    ).finally(() => {
      if (this.#livePending === promise) {
        this.#livePending = undefined;
        this.#refreshing.set(false);
      }
    });
    this.#livePending = promise;
    return promise;
  }
}
