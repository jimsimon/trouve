import type {
  ProtocolClient,
  ProtocolSubscriptionHealth,
} from "./protocol-client.js";
import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

export type SubscriptionHealthFreshness = "if-stale" | "force";

const DEFAULT_TTL_MS = 30_000;
const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;

/** Run one request with a portable, self-clearing deadline. */
export const requestWithDeadline = <T>(
  timeoutMs: number,
  request: (signal: AbortSignal) => Promise<T>,
): Promise<T> => {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  return (async () => request(controller.signal))().finally(() => clearTimeout(timer));
};

/** One app-wide freshness and response-generation gate for provider usage.
 * Some probes launch vendor helpers, so independent component polling is both
 * expensive and vulnerable to older responses overwriting forced refreshes. */
export class SubscriptionHealthController {
  readonly #protocol: Pick<ProtocolClient, "subscriptionHealth">;
  readonly #now: () => number;
  readonly #ttlMs: number;
  readonly #requestTimeoutMs: number;
  readonly #current = createSignal<readonly ProtocolSubscriptionHealth[]>(
    Object.freeze([]),
  );
  readonly current: ReadonlySignal<readonly ProtocolSubscriptionHealth[]> =
    this.#current;
  readonly #loading = createSignal(false);
  /** True only while the newest provider-usage probe is in flight. Catalog
   * consumers use this to reserve the Subscription field without blocking
   * any of the static composer controls. */
  readonly loading: ReadonlySignal<boolean> = this.#loading;

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
    options: {
      readonly now?: () => number;
      readonly ttlMs?: number;
      readonly requestTimeoutMs?: number;
    } = {},
  ) {
    this.#protocol = protocol;
    this.#now = options.now ?? (() => Date.now());
    this.#ttlMs = options.ttlMs ?? DEFAULT_TTL_MS;
    this.#requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
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
    this.#loading.set(true);
    const promise = requestWithDeadline(
      this.#requestTimeoutMs,
      (signal) => this.#protocol.subscriptionHealth(signal),
    ).then(
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
      if (this.#pending?.generation === generation) {
        this.#pending = undefined;
        this.#loading.set(false);
      }
    });
    this.#pending = { generation, promise };
    return promise;
  }
}
