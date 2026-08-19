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
  readonly #liveListeners = new Set<
    (models: readonly ProtocolModelInfo[]) => void
  >();
  readonly #liveLoaded = createSignal(false);
  readonly liveLoaded: ReadonlySignal<boolean> = this.#liveLoaded;
  readonly #refreshing = createSignal(false);
  readonly refreshing: ReadonlySignal<boolean> = this.#refreshing;

  #staticPending: Promise<readonly ProtocolModelInfo[]> | undefined;
  #livePending: Promise<readonly ProtocolModelInfo[]> | undefined;
  #staticLoaded = false;
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
    if (
      freshness === "if-stale"
      && (current.length > 0 || this.#liveLoaded.get())
    ) {
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

  /** Wait for live availability while retaining the static first-paint path. */
  liveModels(
    freshness: ModelCatalogFreshness = "if-stale",
  ): Promise<readonly ProtocolModelInfo[]> {
    const staticOutcome = this.staticModels().then(
      () => ({ ok: true as const }),
      (error: unknown) => ({ ok: false as const, error }),
    );
    return this.#refreshLive(freshness).catch(async (liveError: unknown) => {
      const outcome = await staticOutcome;
      if (!outcome.ok) throw outcome.error;
      throw liveError;
    });
  }

  subscribeLive(
    listener: (models: readonly ProtocolModelInfo[]) => void,
  ): () => void {
    this.#liveListeners.add(listener);
    return () => this.#liveListeners.delete(listener);
  }

  #publishLive(models: readonly ProtocolModelInfo[]): void {
    for (const listener of this.#liveListeners) {
      try {
        listener(models);
      } catch {
        // Consumer failures must not turn successful discovery into a refresh failure.
      }
    }
  }

  #loadStatic(): Promise<readonly ProtocolModelInfo[]> {
    if (this.#staticPending !== undefined) return this.#staticPending;
    const promise = this.#protocol.models().then((models) => {
      const snapshot = Object.freeze([...models]);
      this.#staticLoaded = true;
      this.#static.set(snapshot);
      this.#current.set(this.#liveLoaded.get() ? this.#live.get() : snapshot);
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
        this.#liveLoaded.set(true);
        this.#live.set(snapshot);
        this.#current.set(snapshot);
        this.#publishLive(snapshot);
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
