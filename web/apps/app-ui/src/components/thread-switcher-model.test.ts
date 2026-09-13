import { describe, expect, it } from "vitest";

import {
  durableThreadTabCapacity,
  threadSwitcherRows,
  threadWorkingSet,
  type ThreadSwitcherEntry,
} from "./thread-switcher-model.js";

const entry = (
  id: string,
  parentThreadId?: string,
  closed = false,
): ThreadSwitcherEntry => ({
  id,
  parentThreadId,
  title: id,
  detail: "details for " + id,
  closed,
  pinned: false,
  active: false,
  needsAttention: false,
});

describe("threadSwitcherRows", () => {
  it("renders nested collaborators in stable pre-order", () => {
    const rows = threadSwitcherRows([
      entry("root"),
      entry("child", "root"),
      entry("grandchild", "child"),
      entry("second"),
    ], "");
    expect(rows.map(({ entry: item, depth }) => [item.id, depth])).toEqual([
      ["root", 0],
      ["child", 1],
      ["grandchild", 2],
      ["second", 0],
    ]);
  });

  it("retains ancestors when a descendant matches search", () => {
    const rows = threadSwitcherRows([
      entry("parent"),
      { ...entry("child", "parent"), title: "Needle review" },
      entry("other"),
    ], "needle");
    expect(rows.map(({ entry: item, depth }) => [item.id, depth])).toEqual([
      ["parent", 0],
      ["child", 1],
    ]);
  });

  it("keeps orphaned and cyclic records reachable", () => {
    const rows = threadSwitcherRows([
      entry("orphan", "missing"),
      entry("a", "b"),
      entry("b", "a"),
    ], "");
    expect(new Set(rows.map(({ entry: item }) => item.id))).toEqual(
      new Set(["orphan", "a", "b"]),
    );
  });

  it("handles a deeply nested tree without recursive traversal", () => {
    const entries = Array.from({ length: 10_000 }, (_, index) =>
      entry(String(index), index === 0 ? undefined : String(index - 1)));
    const rows = threadSwitcherRows(entries, "9999");
    expect(rows).toHaveLength(10_000);
    expect(rows.at(-1)).toMatchObject({ depth: 9_999 });
  });

  it("filters by status while retaining matching descendants' ancestors", () => {
    const rows = threadSwitcherRows([
      entry("parent"),
      { ...entry("running", "parent"), active: true },
      { ...entry("attention"), needsAttention: true },
    ], "", "running");
    expect(rows.map(({ entry: item, depth }) => [item.id, depth])).toEqual([
      ["parent", 0],
      ["running", 1],
    ]);
    expect(threadSwitcherRows([
      entry("open"),
      entry("removed", undefined, true),
    ], "", "removed").map(({ entry: item }) => item.id)).toEqual(["removed"]);
  });
});

describe("threadWorkingSet", () => {
  it("reserves the only tab slot for provisional thread setup", () => {
    expect(durableThreadTabCapacity(1, true)).toBe(0);
    expect(threadWorkingSet(["existing"], "existing", [], [], 0)).toEqual([]);
  });
  it("always includes the current thread and fills from recent selections", () => {
    expect(threadWorkingSet(
      ["one", "two", "three", "four"],
      "one",
      [],
      ["three", "two"],
      3,
    )).toEqual(["one", "three", "two"]);
  });

  it("ignores removed and duplicate recent ids", () => {
    expect(threadWorkingSet(
      ["one", "two"],
      "two",
      [],
      ["removed", "two", "one", "one"],
      4,
    )).toEqual(["two", "one"]);
  });

  it("keeps pinned threads first and reserves a slot for the current thread", () => {
    expect(threadWorkingSet(
      ["one", "two", "three", "four"],
      "four",
      ["two", "three"],
      ["one"],
      3,
    )).toEqual(["two", "three", "four"]);
  });

  it("does not reorder tabs when selecting within the working set", () => {
    expect(threadWorkingSet(
      ["one", "two", "three", "four"],
      "two",
      [],
      ["two", "one", "three"],
      3,
      ["one", "two", "three"],
    )).toEqual(["one", "two", "three"]);
  });

  it("replaces an evicted tab in place when selecting a hidden thread", () => {
    expect(threadWorkingSet(
      ["one", "two", "three", "four"],
      "four",
      [],
      ["four", "three", "one"],
      3,
      ["one", "two", "three"],
    )).toEqual(["one", "four", "three"]);
  });
});
