import { describe, expect, it } from "vitest";

import {
  nextHorizontalTabIndex,
  rovingTabIndex,
} from "./tab-navigation.js";

describe("horizontal tab navigation", () => {
  it("keeps only the selected tab in the sequential focus order", () => {
    expect([0, 1, 2].map((index) => rovingTabIndex(index, 1, 3))).toEqual([
      -1,
      0,
      -1,
    ]);
  });

  it("falls back to the first tab while selection is absent or invalid", () => {
    expect([0, 1].map((index) => rovingTabIndex(index, -1, 2))).toEqual([0, -1]);
    expect(rovingTabIndex(0, 4, 2)).toBe(0);
    expect(rovingTabIndex(2, 0, 2)).toBe(-1);
    expect(rovingTabIndex(0, 0, 0)).toBe(-1);
  });

  it("moves left and right with wrapping", () => {
    expect(nextHorizontalTabIndex("ArrowRight", 0, 3)).toBe(1);
    expect(nextHorizontalTabIndex("ArrowRight", 2, 3)).toBe(0);
    expect(nextHorizontalTabIndex("ArrowLeft", 2, 3)).toBe(1);
    expect(nextHorizontalTabIndex("ArrowLeft", 0, 3)).toBe(2);
  });

  it("moves directly to the first and last tabs", () => {
    expect(nextHorizontalTabIndex("Home", 2, 4)).toBe(0);
    expect(nextHorizontalTabIndex("End", 1, 4)).toBe(3);
  });

  it("ignores unrelated keys and invalid tab state", () => {
    expect(nextHorizontalTabIndex("ArrowDown", 1, 3)).toBeUndefined();
    expect(nextHorizontalTabIndex("ArrowRight", -1, 3)).toBeUndefined();
    expect(nextHorizontalTabIndex("ArrowRight", 3, 3)).toBeUndefined();
    expect(nextHorizontalTabIndex("ArrowRight", 0, 0)).toBeUndefined();
  });
});
