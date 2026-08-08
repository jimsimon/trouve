import type {
  ProtocolClient,
  ProtocolSubscriptionHealth,
} from "./protocol-client.js";
import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

export type SubscriptionHealthFreshness = "if-stale" | "force";

const DEFAULT_TTL_MS = 30_000;

/** One app-wide freshness and response-generation gate for provider usage.
 * Some probes launch vendor helpers, so independent component polling is both
 * expensive and vulnerable to older responses overwriting forced refreshes. */
export class SubscriptionHealthController {
  readonly #protocol: Pick<ProtocolClient, "subscriptionHealth">;
  readonly #now: () => number;
  readonly #ttlMs: number;
  readonly #current = createSignal<readonly ProtocolSubscriptionHealth[]>(
    Object.freeze([]),
  );
  readonly current: ReadonlySignal<readonly ProtocolSubscriptionHealth[]> =
    this.#current;

  #lastStartedAt: number | undefined;
  #generation = 0;
  #pending:
    | {
        readonly generation: number;
        readonly promise: Promise<readonly ProtocolSubscriptionHealth[]>;
      }
    | undefined;

  constructor(
    protocol: Pick<ProtocolClient, "subscriptionHealth">,
    options: { readonly now?: () => number; readonly ttlMs?: number } = {},
  ) {
    this.#protocol = protocol;
    this.#now = options.now ?? (() => Date.now());
    this.#ttlMs = options.ttlMs ?? DEFAULT_TTL_MS;
  }

  refresh(
    freshness: SubscriptionHealthFreshness = "if-stale",
  ): Promise<readonly ProtocolSubscriptionHealth[]> {
    const now = this.#now();
    const fresh = this.#lastStartedAt !== undefined &&
      Math.max(0, now - this.#lastStartedAt) < this.#ttlMs;
    if (freshness === "if-stale" && fresh) {
      return this.#pending?.promise ?? Promise.resolve(this.#current.get());
    }

    this.#lastStartedAt = now;
    const generation = ++this.#generation;
    const promise = this.#protocol.subscriptionHealth().then(
      (health) => {
        if (generation === this.#generation) {
          const snapshot = Object.freeze([...health]);
          this.#current.set(snapshot);
          return snapshot;
        }
        return this.#current.get();
      },
      (error: unknown) => {
        // A force refresh invalidates both successful and failed older
        // responses. Only the newest generation may surface a diagnostic.
        if (generation !== this.#generation) return this.#current.get();
        throw error;
      },
    ).finally(() => {
      if (this.#pending?.generation === generation) this.#pending = undefined;
    });
    this.#pending = { generation, promise };
    return promise;
  }
}
