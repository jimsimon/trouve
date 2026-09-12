const STORAGE_KEY = "trouve.code-review-group-order.v1";
const MAX_GROUPS = 256;
const MAX_REPOSITORY_LENGTH = 512;

export interface CodeReviewGroupOrderStorage {
  load(): readonly string[] | undefined;
  /** False means persistence was unavailable; the in-memory order remains usable. */
  save(order: readonly string[]): boolean;
}

export const normalizeCodeReviewGroupOrder = (
  value: unknown,
): readonly string[] | undefined => {
  if (!Array.isArray(value)) return undefined;
  const seen = new Set<string>();
  const order: string[] = [];
  for (const candidate of value.slice(0, MAX_GROUPS)) {
    if (typeof candidate !== "string") continue;
    const repository = candidate.trim();
    if (
      repository === "" ||
      repository.length > MAX_REPOSITORY_LENGTH ||
      seen.has(repository)
    ) continue;
    seen.add(repository);
    order.push(repository);
  }
  return order;
};

export const browserCodeReviewGroupOrderStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): CodeReviewGroupOrderStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      if (raw === null) return undefined;
      return normalizeCodeReviewGroupOrder(JSON.parse(raw) as unknown);
    } catch {
      return undefined;
    }
  },
  save: (order) => {
    try {
      storage.setItem(
        STORAGE_KEY,
        JSON.stringify(normalizeCodeReviewGroupOrder(order) ?? []),
      );
      return true;
    } catch {
      return false;
    }
  },
});

export const createBrowserCodeReviewGroupOrderStorage = ():
  CodeReviewGroupOrderStorage | undefined => {
  try {
    return globalThis.localStorage === undefined
      ? undefined
      : browserCodeReviewGroupOrderStorage(globalThis.localStorage);
  } catch {
    return undefined;
  }
};
