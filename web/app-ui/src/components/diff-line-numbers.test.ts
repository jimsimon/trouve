import { describe, expect, it } from "vitest";

import {
  diffLineRangeDescription,
  formatMappedLineNumber,
} from "./diff-line-numbers.js";

describe("diff line numbers", () => {
  it("maps display rows to source lines while leaving synthetic gaps blank", () => {
    const lines = [8, 9, null, 42] as const;
    expect(formatMappedLineNumber(1, lines)).toBe("8");
    expect(formatMappedLineNumber(3, lines)).toBe("");
    expect(formatMappedLineNumber(4, lines)).toBe("42");
    expect(formatMappedLineNumber(5, lines)).toBe("");
  });

  it("describes both source ranges for assistive technology", () => {
    expect(diffLineRangeDescription([8, null, 42], [9, null, 43])).toBe(
      "Original excerpt lines 8–42; modified excerpt lines 9–43.",
    );
    expect(diffLineRangeDescription([], [7])).toBe(
      "Original line positions unavailable; modified excerpt line 7.",
    );
  });
});
