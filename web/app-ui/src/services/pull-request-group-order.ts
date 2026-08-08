import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

const STORAGE_KEY = "trouve.pull-request-group-order.v1";
const MAX_GROUPS = 32;
const VALID_GROUP_KEY = /^[a-z][a-z0-9_-]{0,63}$/u;

export interface PullRequestGroupOrderStorage {
  load(): readonly string[];
  save(order: readonly string[]): void;
}

export const normalizePullRequestGroupOrder = (value: unknown): readonly string[] => {
  if (!Array.isArray(value)) return Object.freeze([]);
  const seen = new Set<string>();
  const order: string[] = [];
  for (const entry of value.slice(0, MAX_GROUPS)) {
    if (
      typeof entry !== "string" ||
      !VALID_GROUP_KEY.test(entry) ||
      seen.has(entry)
    ) continue;
    seen.add(entry);
    order.push(entry);
  }
  return Object.freeze(order);
};

export const browserPullRequestGroupOrderStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): PullRequestGroupOrderStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      return raw === null
        ? Object.freeze([])
        : normalizePullRequestGroupOrder(JSON.parse(raw));
    } catch {
      return Object.freeze([]);
    }
  },
  save: (order) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(normalizePullRequestGroupOrder(order)));
    } catch {
      // The in-memory order remains usable for this frontend lifetime.
    }
  },
});

export class PullRequestGroupOrderController {
  readonly #storage: PullRequestGroupOrderStorage | undefined;
  readonly #order = createSignal<readonly string[]>(Object.freeze([]));
  readonly order: ReadonlySignal<readonly string[]> = this.#order;

  constructor(storage?: PullRequestGroupOrderStorage) {
    this.#storage = storage;
    this.#order.set(normalizePullRequestGroupOrder(storage?.load()));
  }

  replace(value: unknown, persist = true): readonly string[] {
    const next = normalizePullRequestGroupOrder(value);
    const current = this.#order.get();
    if (
      next.length === current.length &&
      next.every((key, index) => key === current[index])
    ) return current;
    this.#order.set(next);
    if (persist) this.#storage?.save(next);
    return next;
  }
}

export const createBrowserPullRequestGroupOrderController = (
  persistLocally: boolean,
): PullRequestGroupOrderController => {
  if (!persistLocally) return new PullRequestGroupOrderController();
  try {
    return new PullRequestGroupOrderController(
      browserPullRequestGroupOrderStorage(globalThis.localStorage),
    );
  } catch {
    return new PullRequestGroupOrderController();
  }
};
