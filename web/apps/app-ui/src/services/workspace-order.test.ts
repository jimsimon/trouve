import { describe, expect, it, vi } from "vitest";

import {
  browserWorkspaceOrderStorage,
  normalizeWorkspaceOrder,
  WorkspaceOrderController,
} from "./workspace-order.js";

const workspaces = [
  { id: "a", name: "A" },
  { id: "b", name: "B" },
  { id: "c", name: "C" },
];

describe("workspace order", () => {
  it("normalizes untrusted state and reconciles added/removed workspaces", () => {
    expect(normalizeWorkspaceOrder(["b", "b", "", 1, "a"])).toEqual(["b", "a"]);
    const save = vi.fn();
    const controller = new WorkspaceOrderController({ load: () => ["b", "gone"], save });
    expect(controller.reconcile(workspaces).map(({ id }) => id)).toEqual(["b", "a", "c"]);
    expect(save).toHaveBeenLastCalledWith(["b", "a", "c"]);
  });

  it("supports bounded adjacent moves and before/after drops", () => {
    const controller = new WorkspaceOrderController();
    controller.replace(["a", "b", "c"], false);
    expect(controller.move("b", -1)).toBe(true);
    expect(controller.order.get()).toEqual(["b", "a", "c"]);
    expect(controller.move("b", -1)).toBe(false);
    expect(controller.drop("b", "c", true)).toBe(true);
    expect(controller.order.get()).toEqual(["a", "c", "b"]);
  });

  it("round-trips browser storage without surfacing storage failures", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => values.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => values.set(key, value)),
    };
    const adapter = browserWorkspaceOrderStorage(storage);
    adapter.save(["c", "a"]);
    expect(adapter.load()).toEqual(["c", "a"]);
    storage.getItem.mockImplementation(() => "not-json");
    expect(adapter.load()).toEqual([]);
  });
});
