import { describe, expect, it, vi } from "vitest";

import {
  browserCodeReviewGroupOrderStorage,
  normalizeCodeReviewGroupOrder,
} from "./code-review-group-order-preferences.js";

describe("code-review group order preferences", () => {
  it("round-trips a bounded, client-owned repository order", () => {
    let value: string | null = null;
    const setItem = vi.fn((_key: string, next: string) => {
      value = next;
    });
    const storage = browserCodeReviewGroupOrderStorage({
      getItem: () => value,
      setItem,
    });

    expect(storage.load()).toBeUndefined();
    expect(storage.save(["trouve/zeta", "trouve/alpha"])).toBe(true);
    expect(storage.load()).toEqual(["trouve/zeta", "trouve/alpha"]);
    expect(setItem).toHaveBeenCalledWith(
      "trouve.code-review-group-order.v1",
      '["trouve/zeta","trouve/alpha"]',
    );
  });

  it("normalizes duplicates and unsafe or malformed stored values", () => {
    expect(normalizeCodeReviewGroupOrder([
      " trouve/app ",
      "trouve/app",
      "",
      42,
      "x".repeat(513),
      "trouve/search",
    ])).toEqual(["trouve/app", "trouve/search"]);
    expect(normalizeCodeReviewGroupOrder({ order: ["trouve/app"] })).toBeUndefined();

    const malformed = browserCodeReviewGroupOrderStorage({
      getItem: () => "not-json",
      setItem: () => undefined,
    });
    expect(malformed.load()).toBeUndefined();
  });

  it("contains unavailable storage without breaking in-memory reordering", () => {
    const storage = browserCodeReviewGroupOrderStorage({
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("quota");
      },
    });

    expect(storage.load()).toBeUndefined();
    expect(storage.save(["trouve/app"])).toBe(false);
  });
});
