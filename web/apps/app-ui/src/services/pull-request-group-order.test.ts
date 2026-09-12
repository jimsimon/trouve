import { describe, expect, it, vi } from "vitest";

import {
  browserPullRequestGroupOrderStorage,
  normalizePullRequestGroupOrder,
  PullRequestGroupOrderController,
} from "./pull-request-group-order.js";

describe("pull request group order", () => {
  it("normalizes bounded untrusted group keys", () => {
    expect(normalizePullRequestGroupOrder([
      "ready-to-merge",
      "ready-to-merge",
      "",
      "UPPER",
      42,
      "drafts",
    ])).toEqual(["ready-to-merge", "drafts"]);
  });

  it("persists only real order changes", () => {
    const save = vi.fn();
    const controller = new PullRequestGroupOrderController({
      load: () => ["drafts"],
      save,
    });
    expect(controller.replace(["drafts"])).toBe(controller.order.get());
    expect(save).not.toHaveBeenCalled();
    controller.replace(["ready-to-merge", "drafts"]);
    expect(save).toHaveBeenCalledWith(["ready-to-merge", "drafts"]);
  });

  it("round-trips browser storage and contains storage failures", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => values.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => values.set(key, value)),
    };
    const adapter = browserPullRequestGroupOrderStorage(storage);
    adapter.save(["pending-review", "drafts"]);
    expect(adapter.load()).toEqual(["pending-review", "drafts"]);
    storage.getItem.mockImplementation(() => "invalid-json");
    expect(adapter.load()).toEqual([]);
  });
});
