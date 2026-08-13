import { describe, expect, it } from "vitest";

import { lineRangeOffsets, parentDirectories } from "./file-reveal.js";

describe("file reveal helpers", () => {
  it("maps one-based inclusive lines to UTF-16 editor offsets", () => {
    const content = "alpha\n😀 beta\ngamma\ndelta";
    expect(lineRangeOffsets(content, 2, 3)).toEqual({
      from: 6,
      to: 19,
    });
    expect(content.slice(6, 19)).toBe("😀 beta\ngamma");
  });

  it("clamps stale ranges and treats zero as no selection", () => {
    expect(lineRangeOffsets("one\ntwo", 99, 120)).toEqual({ from: 4, to: 7 });
    expect(lineRangeOffsets("one", 0, 0)).toEqual({ from: 0, to: 0 });
  });

  it("lists relative parent directories from shallow to deep", () => {
    expect(parentDirectories("./crates/app/src/main.rs")).toEqual([
      "crates",
      "crates/app",
      "crates/app/src",
    ]);
    expect(parentDirectories("README.md")).toEqual([]);
  });
});
