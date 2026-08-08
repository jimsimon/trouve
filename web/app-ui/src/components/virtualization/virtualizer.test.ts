import { describe, expect, it } from "vitest";

import { Virtualizer, type VirtualItem } from "./virtualizer.js";

const items = (count: number, options: Partial<VirtualItem> = {}): VirtualItem[] =>
  Array.from({ length: count }, (_, index) => ({ id: `item-${index}`, ...options }));

describe("Virtualizer", () => {
  it("builds an overscanned window from fixed and variable estimates", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 10 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems([
      { id: "a", estimatedHeight: 10 },
      { id: "b", estimatedHeight: 30 },
      { id: "c" },
      { id: "d" },
      { id: "e" },
    ]);
    virtualizer.setViewport(35, 20);
    const window = virtualizer.window();
    expect(window.items.map(({ item }) => item.id)).toEqual(["b", "c", "d"]);
    expect(window.paddingBefore).toBe(10);
    expect(window.paddingAfter).toBe(20);
    expect(window.totalHeight).toBe(100);
  });

  it("corrects scroll when a measured item above the anchor changes", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(10));
    virtualizer.setViewport(85, 40);
    const changed = virtualizer.measure("item-1", 35);
    expect(changed.delta).toBe(15);
    expect(changed.scrollTop).toBe(100);
    expect(virtualizer.window().items[0]?.item.id).toBe("item-4");
  });

  it("updates row geometry when the anchored item changes without moving scroll", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 100 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(6));
    virtualizer.setViewport(45, 40);

    const changed = virtualizer.measure("item-2", 50);
    expect(changed.delta).toBe(0);
    expect(changed.scrollTop).toBe(45);
    expect(virtualizer.window().totalHeight).toBe(150);
    expect(
      virtualizer.window().items.find(({ item }) => item.id === "item-3")?.start,
    ).toBe(90);
  });

  it("preserves a stable visible anchor when items are prepended and the tail grows", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(8));
    virtualizer.setViewport(65, 40);
    const changed = virtualizer.setItems([
      { id: "new-a" },
      { id: "new-b" },
      ...items(8),
      { id: "new-tail", estimatedHeight: 200 },
    ]);
    expect(changed.delta).toBe(40);
    expect(changed.scrollTop).toBe(105);
    expect(virtualizer.window().items[0]?.item.id).toBe("item-3");
    expect(virtualizer.window().totalHeight).toBe(400);
  });

  it("parks history after scrolling away and resumes following at the exact tail", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(4));
    expect(virtualizer.window().scrollTop).toBe(40);
    virtualizer.setViewport(0, 40, { userInitiated: true });
    expect(virtualizer.window().followingTail).toBe(false);
    virtualizer.setItems(items(6));
    expect(virtualizer.window().scrollTop).toBe(0);

    virtualizer.setViewport(80, 40, { userInitiated: true });
    expect(virtualizer.window().followingTail).toBe(true);
    virtualizer.setItems(items(7));
    expect(virtualizer.window().scrollTop).toBe(100);
  });

  it("trusts the rendered viewport when measured and estimated tails differ", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(6));
    virtualizer.setViewport(62, 40, { userInitiated: true, atTail: true });
    expect(virtualizer.window().followingTail).toBe(true);
  });

  it("honors an intentional near-tail scroll without snapping it back", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(10));
    virtualizer.setViewport(152, 40, { userInitiated: true });
    expect(virtualizer.window().scrollTop).toBe(152);
    expect(virtualizer.window().followingTail).toBe(false);
    virtualizer.setItems(items(11));
    expect(virtualizer.window().scrollTop).toBe(152);
  });

  it("keeps the live tail anchored when the viewport changes height", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(10));
    expect(virtualizer.window().scrollTop).toBe(160);

    expect(virtualizer.resizeViewport(80).scrollTop).toBe(120);
    expect(virtualizer.window().followingTail).toBe(true);
    expect(virtualizer.resizeViewport(20).scrollTop).toBe(180);
    expect(virtualizer.window().followingTail).toBe(true);

    virtualizer.setViewport(75, 20);
    expect(virtualizer.window().followingTail).toBe(false);
    expect(virtualizer.resizeViewport(60).scrollTop).toBe(75);
  });

  it("round-trips a stable parked-history bookmark and clears it at the tail", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    virtualizer.setItems(items(10));
    virtualizer.setViewport(67, 40);
    expect(virtualizer.bookmark()).toEqual({ id: "item-3", offset: 7 });

    const restored = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    restored.setViewport(0, 40);
    restored.setItems(items(10));
    expect(restored.restoreBookmark({ id: "item-3", offset: 7 }).scrollTop).toBe(67);
    expect(restored.window().followingTail).toBe(false);
    expect(restored.bookmark()).toEqual({ id: "item-3", offset: 7 });
    restored.enableFollowTail();
    expect(restored.bookmark()).toBeUndefined();
  });

  it("retains a bookmark until its item arrives and rejects invalid offsets", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 40);
    expect(virtualizer.restoreBookmark({ id: "later", offset: 5 }).scrollTop).toBe(0);
    virtualizer.setItems([{ id: "first" }, { id: "later" }, { id: "last" }]);
    expect(virtualizer.window().scrollTop).toBe(20);
    expect(() => virtualizer.restoreBookmark({ id: "later", offset: -1 })).toThrow(
      /non-negative/,
    );
  });

  it("offers a nonvirtual accessibility mode and unmounts offscreen heavy widgets", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20, overscanPx: 0 });
    virtualizer.setViewport(0, 20);
    virtualizer.setItems([
      { id: "terminal", heavyweight: true },
      { id: "message" },
      { id: "diff", heavyweight: true },
    ]);
    virtualizer.setViewport(20, 20);
    expect(virtualizer.shouldUnmountHeavyweight("terminal")).toBe(true);
    expect(virtualizer.shouldUnmountHeavyweight("diff")).toBe(true);
    virtualizer.setMode("accessible");
    expect(virtualizer.window().items).toHaveLength(3);
    expect(virtualizer.shouldUnmountHeavyweight("terminal")).toBe(false);
  });

  it("rejects duplicate stable ids and invalid measurements", () => {
    const virtualizer = new Virtualizer({ estimatedHeight: 20 });
    expect(() => virtualizer.setItems([{ id: "" }])).toThrow(/must not be empty/);
    expect(() => virtualizer.setItems([{ id: "same" }, { id: "same" }])).toThrow(
      /duplicate/,
    );
    virtualizer.setItems([{ id: "one" }]);
    expect(() => virtualizer.measure("one", 0)).toThrow(/positive/);
    expect(() => virtualizer.measure("missing", 20)).toThrow(/unknown/);
  });
});
